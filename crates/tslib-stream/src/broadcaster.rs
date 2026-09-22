//! One WebRTC peer connection per viewer, all fed from one encode.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use rtc::interceptor::Registry;
use rtc::media::Sample;
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::media_engine::MIME_TYPE_VP8;
use rtc::rtp_transceiver::rtp_sender::{
    RTCPFeedback, RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters,
    RTCRtpEncodingParameters, RtpCodecKind,
};
use rtc::rtp_transceiver::{RTCRtpTransceiverDirection, RTCRtpTransceiverInit};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::peer_connection::{
    register_default_interceptors, MediaEngine, PeerConnection, PeerConnectionBuilder,
    PeerConnectionEventHandler, RTCConfigurationBuilder, RTCIceCandidateInit,
    RTCIceCandidateType, RTCIceGatheringState, RTCIceServer, RTCPeerConnectionIceEvent,
    RTCPeerConnectionState, RTCSessionDescription,
};

use crate::encoder::{EncoderConfig, VideoEncoder, VideoInput};
use crate::{Error, Result};

/// Payload type the desktop client's own offer uses for VP8.
const VP8_PT: u8 = 96;

/// How the broadcaster reaches its viewers.
#[derive(Debug, Clone)]
pub struct BroadcastConfig {
    pub encoder: EncoderConfig,
    /// STUN servers. The default is the pair hardcoded in the desktop client;
    /// neither relays, so there is no TURN.
    pub ice_servers: Vec<String>,
    /// Local UDP addresses to gather candidates on. A wildcard binds every
    /// interface — Docker bridges included, whose STUN requests only ever time
    /// out — so name the real interface when there is a choice.
    pub bind_addrs: Vec<String>,
    /// The longest to wait for ICE gathering before sending the offer anyway.
    pub gather_timeout: Duration,
}

impl Default for BroadcastConfig {
    fn default() -> Self {
        Self {
            encoder: EncoderConfig::default(),
            ice_servers: vec![
                "stun:turn.teamspeak.com:3478".into(),
                "stun:turn2.teamspeak.com:3478".into(),
            ],
            bind_addrs: vec!["0.0.0.0:0".into()],
            gather_timeout: Duration::from_secs(3),
        }
    }
}

/// Something the caller has to act on, or may want to report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BroadcastEvent {
    /// Send this with `Client::accept_stream_viewer`.
    Offer { viewer_id: u16, sdp: String },
    /// Media is flowing to this viewer.
    Connected { viewer_id: u16 },
    /// The peer connection is gone: failed, closed, or never answered.
    Gone { viewer_id: u16, reason: String },
}

/// Server → viewer task.
enum ViewerMsg {
    Answer(String),
    Candidate(RTCIceCandidateInit),
}

/// Viewer task → broadcaster. `session` tells a replaced peer connection's
/// last words apart from the current one's.
struct Report {
    viewer_id: u16,
    session: u64,
    event: BroadcastEvent,
}

struct Viewer {
    session: u64,
    tx: mpsc::UnboundedSender<ViewerMsg>,
}

/// The media side of one published stream.
///
/// Not tied to a connection: the caller relays join requests and signaling
/// from `tslib_core` in, and [`BroadcastEvent::Offer`]s back out. Needs a
/// Tokio runtime, since every viewer runs on a task of its own.
pub struct Broadcaster {
    config: BroadcastConfig,
    frames: broadcast::Sender<Bytes>,
    encoder: Option<VideoEncoder>,
    viewers: HashMap<u16, Viewer>,
    next_session: u64,
    reports_tx: mpsc::UnboundedSender<Report>,
    reports_rx: mpsc::UnboundedReceiver<Report>,
}

impl Broadcaster {
    pub fn new(config: BroadcastConfig) -> Self {
        // Two seconds of frames: a viewer further behind than that skips to
        // the next keyframe instead.
        let (frames, _) = broadcast::channel(config.encoder.fps as usize * 2);
        let (reports_tx, reports_rx) = mpsc::unbounded_channel();
        Self {
            config,
            frames,
            encoder: None,
            viewers: HashMap::new(),
            next_session: 0,
            reports_tx,
            reports_rx,
        }
    }

    /// Replace whatever is playing with `input`. Connected viewers stay
    /// connected and pick up the new video at its first keyframe.
    pub fn play(&mut self, input: VideoInput) -> Result<()> {
        self.encoder = None;
        self.encoder = Some(VideoEncoder::spawn(input, self.config.encoder, self.frames.clone())?);
        Ok(())
    }

    /// Stop the video. Viewers stay connected and keep the last frame.
    pub fn stop_video(&mut self) {
        self.encoder = None;
    }

    /// Freeze or resume the video exactly where it is.
    pub fn set_paused(&mut self, paused: bool) {
        if let Some(encoder) = &self.encoder {
            encoder.set_paused(paused);
        }
    }

    /// Whether the current video has put out its first frame.
    pub fn video_started(&self) -> bool {
        self.encoder.as_ref().is_some_and(VideoEncoder::has_started)
    }

    /// Whether the current video ran out or failed. `false` when idle.
    pub fn video_finished(&self) -> bool {
        self.encoder.as_ref().is_some_and(VideoEncoder::has_finished)
    }

    /// Whether a video is loaded (playing or paused).
    pub fn has_video(&self) -> bool {
        self.encoder.is_some()
    }

    pub fn viewer_count(&self) -> usize {
        self.viewers.len()
    }

    /// Start a peer connection for a viewer that asked to join. The offer
    /// comes back as [`BroadcastEvent::Offer`]. A repeated request replaces
    /// the previous peer connection.
    pub fn add_viewer(&mut self, viewer_id: u16) {
        self.next_session += 1;
        let session = self.next_session;
        let (tx, rx) = mpsc::unbounded_channel();
        self.viewers.insert(viewer_id, Viewer { session, tx });
        let frames = self.frames.subscribe();
        let reports = self.reports_tx.clone();
        let config = self.config.clone();
        tokio::spawn(async move {
            let report = |event| {
                let _ = reports.send(Report { viewer_id, session, event });
            };
            let reason = match run_viewer(viewer_id, &config, frames, rx, &report).await {
                Ok(()) => "removed".to_string(),
                Err(error) => {
                    warn!(viewer_id, %error, "Viewer failed");
                    error.to_string()
                }
            };
            report(BroadcastEvent::Gone { viewer_id, reason });
        });
    }

    /// Drop a viewer: it left, was kicked, or disconnected. Returns whether
    /// it was there.
    pub fn remove_viewer(&mut self, viewer_id: u16) -> bool {
        // Dropping the sender ends the viewer's task.
        self.viewers.remove(&viewer_id).is_some()
    }

    /// Drop every viewer, e.g. when the stream stops.
    pub fn clear_viewers(&mut self) {
        self.viewers.clear();
    }

    /// Feed a `notifystreamsignaling` payload from a viewer: its answer or a
    /// trickled ICE candidate. Anything else is ignored.
    pub fn handle_signaling(&mut self, viewer_id: u16, json: &str) {
        let Some(viewer) = self.viewers.get(&viewer_id) else { return };
        match parse_signaling(json) {
            Some(msg) => {
                let _ = viewer.tx.send(msg);
            }
            None => debug!(viewer_id, "Ignoring signaling message"),
        }
    }

    /// The next thing to act on, if any. Call it regularly.
    pub fn poll_event(&mut self) -> Option<BroadcastEvent> {
        while let Ok(report) = self.reports_rx.try_recv() {
            let current = self
                .viewers
                .get(&report.viewer_id)
                .is_some_and(|v| v.session == report.session);
            match report.event {
                BroadcastEvent::Gone { .. } => {
                    if current {
                        self.viewers.remove(&report.viewer_id);
                        return Some(report.event);
                    }
                }
                _ if current => return Some(report.event),
                _ => {}
            }
        }
        None
    }
}

/// `{"cmd":"answer","args":{"answer":…}}` or
/// `{"cmd":"iceCandidate","args":{"mLine":…,"mid":…,"sdp":…}}`.
fn parse_signaling(json: &str) -> Option<ViewerMsg> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let args = value.get("args")?;
    match value.get("cmd")?.as_str()? {
        "answer" => Some(ViewerMsg::Answer(args.get("answer")?.as_str()?.to_string())),
        "iceCandidate" => Some(ViewerMsg::Candidate(RTCIceCandidateInit {
            candidate: args.get("sdp")?.as_str()?.to_string(),
            sdp_mid: args.get("mid").and_then(|m| m.as_str()).map(str::to_string),
            sdp_mline_index: args
                .get("mLine")
                .and_then(serde_json::Value::as_u64)
                .and_then(|m| u16::try_from(m).ok()),
            ..Default::default()
        })),
        _ => None,
    }
}

enum Gathering {
    Complete,
    /// A server-reflexive candidate: enough to cross a NAT.
    Reflexive,
}

#[derive(Clone)]
struct Handler {
    gathering: mpsc::UnboundedSender<Gathering>,
    state: mpsc::UnboundedSender<RTCPeerConnectionState>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        if event.candidate.typ == RTCIceCandidateType::Srflx {
            let _ = self.gathering.send(Gathering::Reflexive);
        }
    }

    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            let _ = self.gathering.send(Gathering::Complete);
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        let _ = self.state.send(state);
    }
}

fn vp8_codec() -> RTCRtpCodec {
    RTCRtpCodec {
        mime_type: MIME_TYPE_VP8.to_owned(),
        clock_rate: 90000,
        channels: 0,
        sdp_fmtp_line: String::new(),
        rtcp_feedback: vec![
            RTCPFeedback { typ: "nack".into(), parameter: String::new() },
            RTCPFeedback { typ: "nack".into(), parameter: "pli".into() },
            RTCPFeedback { typ: "ccm".into(), parameter: "fir".into() },
        ],
    }
}

/// VP8 payload header: bit 0 of the first byte is 0 on a keyframe.
fn is_keyframe(frame: &[u8]) -> bool {
    frame.first().is_some_and(|b| b & 0x01 == 0)
}

/// `RandomState` is seeded per instance, which is all an SSRC needs.
fn random_ssrc() -> u32 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new().build_hasher().finish() as u32
}

async fn run_viewer(
    viewer_id: u16,
    config: &BroadcastConfig,
    mut frames: broadcast::Receiver<Bytes>,
    mut rx: mpsc::UnboundedReceiver<ViewerMsg>,
    report: &impl Fn(BroadcastEvent),
) -> Result<()> {
    let mut media_engine = MediaEngine::default();
    media_engine.register_codec(
        RTCRtpCodecParameters { rtp_codec: vp8_codec(), payload_type: VP8_PT, ..Default::default() },
        RtpCodecKind::Video,
    )?;
    let registry = register_default_interceptors(Registry::new(), &mut media_engine)?;
    let rtc_config = RTCConfigurationBuilder::new()
        .with_ice_servers(vec![RTCIceServer {
            urls: config.ice_servers.clone(),
            ..Default::default()
        }])
        .build();

    let (gathering_tx, mut gathering_rx) = mpsc::unbounded_channel();
    let (state_tx, mut state_rx) = mpsc::unbounded_channel();
    let pc = PeerConnectionBuilder::new()
        .with_configuration(rtc_config)
        .with_media_engine(media_engine)
        .with_interceptor_registry(registry)
        .with_handler(Arc::new(Handler { gathering: gathering_tx, state: state_tx }))
        .with_udp_addrs(config.bind_addrs.clone())
        .build()
        .await?;

    // One video m-line, send-only. The desktop client also offers an inactive
    // audio line, but it accepts an offer without one, and webrtc-rs cannot
    // create an inactive transceiver.
    let ssrc = random_ssrc();
    let track = Arc::new(TrackLocalStaticSample::new(
        Instant::now(),
        MediaStreamTrack::new(
            "outgoing_video".to_owned(),
            format!("video-{viewer_id}"),
            "video".to_owned(),
            RtpCodecKind::Video,
            vec![RTCRtpEncodingParameters {
                rtp_coding_parameters: RTCRtpCodingParameters { ssrc: Some(ssrc), ..Default::default() },
                codec: vp8_codec(),
                ..Default::default()
            }],
        ),
    )?);
    pc.add_transceiver_from_track(
        Arc::clone(&track) as Arc<dyn TrackLocal>,
        Some(RTCRtpTransceiverInit {
            direction: RTCRtpTransceiverDirection::Sendonly,
            ..Default::default()
        }),
    )
    .await?;

    let offer = pc.create_offer(None).await?;
    pc.set_local_description(offer).await?;
    wait_for_candidates(&mut gathering_rx, config.gather_timeout).await;
    // Non-trickle: every candidate so far rides in the SDP, so a join costs
    // one command and cannot trip flood protection.
    let Some(local) = pc.local_description().await else {
        let _ = pc.close().await;
        return Err(Error::PeerConnection("has no local description".into()));
    };
    report(BroadcastEvent::Offer { viewer_id, sdp: local.sdp });

    let result = stream_to_viewer(viewer_id, &pc, &track, ssrc, &mut frames, &mut rx, &mut state_rx, report).await;
    let _ = pc.close().await;
    result
}

/// Waits for gathering to finish, or for a reflexive candidate plus a short
/// grace period — whichever comes first — bounded by `timeout`. Interfaces
/// that cannot reach a STUN server (Docker bridges) otherwise hold the offer
/// for the whole timeout.
async fn wait_for_candidates(gathering_rx: &mut mpsc::UnboundedReceiver<Gathering>, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut grace: Option<tokio::time::Instant> = None;
    loop {
        let until = grace.map_or(deadline, |g| g.min(deadline));
        match tokio::time::timeout_at(until, gathering_rx.recv()).await {
            Ok(Some(Gathering::Complete)) | Ok(None) | Err(_) => return,
            Ok(Some(Gathering::Reflexive)) => {
                grace.get_or_insert(tokio::time::Instant::now() + Duration::from_millis(300));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn stream_to_viewer(
    viewer_id: u16,
    pc: &impl PeerConnection,
    track: &TrackLocalStaticSample,
    ssrc: u32,
    frames: &mut broadcast::Receiver<Bytes>,
    rx: &mut mpsc::UnboundedReceiver<ViewerMsg>,
    state_rx: &mut mpsc::UnboundedReceiver<RTCPeerConnectionState>,
    report: &impl Fn(BroadcastEvent),
) -> Result<()> {
    let mut answered = false;
    let mut pending: Vec<RTCIceCandidateInit> = Vec::new();
    let mut connected = false;
    let mut need_keyframe = true;
    let mut last_frame: Option<Instant> = None;

    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Some(ViewerMsg::Answer(sdp)) => {
                    pc.set_remote_description(RTCSessionDescription::answer(sdp)?).await?;
                    answered = true;
                    for candidate in pending.drain(..) {
                        pc.add_ice_candidate(candidate).await?;
                    }
                }
                Some(ViewerMsg::Candidate(candidate)) => {
                    if answered {
                        pc.add_ice_candidate(candidate).await?;
                    } else {
                        pending.push(candidate);
                    }
                }
                // Removed by the broadcaster.
                None => return Ok(()),
            },
            Some(state) = state_rx.recv() => {
                debug!(viewer_id, %state, "Peer connection");
                match state {
                    RTCPeerConnectionState::Connected if !connected => {
                        // Frames queued since the join request are stale:
                        // start from the live edge.
                        *frames = frames.resubscribe();
                        connected = true;
                        info!(viewer_id, "Viewer connected");
                        report(BroadcastEvent::Connected { viewer_id });
                    }
                    RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed => {
                        return Err(Error::PeerConnection(state.to_string()));
                    }
                    _ => {}
                }
            }
            frame = frames.recv(), if connected => match frame {
                Ok(frame) => {
                    if need_keyframe {
                        if !is_keyframe(&frame) {
                            continue;
                        }
                        need_keyframe = false;
                    }
                    // The real gap between frames, so a pause or a source
                    // switch does not squeeze the RTP clock.
                    let now = Instant::now();
                    let duration = last_frame.map_or(Duration::from_millis(33), |t| now - t);
                    last_frame = Some(now);
                    track
                        .sample_writer(ssrc, VP8_PT)
                        .write_sample(&Sample { data: frame, duration, ..Sample::new(now) })
                        .await?;
                }
                Err(broadcast::error::RecvError::Lagged(dropped)) => {
                    warn!(viewer_id, dropped, "Viewer fell behind, waiting for a keyframe");
                    need_keyframe = true;
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answer_and_candidates_parse() {
        let Some(ViewerMsg::Answer(sdp)) =
            parse_signaling(r#"{"cmd":"answer","args":{"answer":"v=0\r\n"}}"#)
        else {
            panic!("expected an answer");
        };
        assert_eq!(sdp, "v=0\r\n");

        let Some(ViewerMsg::Candidate(candidate)) = parse_signaling(
            r#"{"args":{"mLine":0,"mid":"0","sdp":"candidate:1 1 udp 1 10.0.0.1 9 typ host"},"cmd":"iceCandidate"}"#,
        ) else {
            panic!("expected a candidate");
        };
        assert_eq!(candidate.candidate, "candidate:1 1 udp 1 10.0.0.1 9 typ host");
        assert_eq!(candidate.sdp_mid.as_deref(), Some("0"));
        assert_eq!(candidate.sdp_mline_index, Some(0));
    }

    #[test]
    fn unknown_or_broken_signaling_is_ignored() {
        for json in ["not-json", r#"{"cmd":"bogus","args":{}}"#, r#"{"cmd":"answer","args":{}}"#, "{}"] {
            assert!(parse_signaling(json).is_none(), "{json}");
        }
    }

    #[test]
    fn vp8_keyframe_bit() {
        assert!(is_keyframe(&[0x10, 0x02]));
        assert!(!is_keyframe(&[0x11, 0x02]));
        assert!(!is_keyframe(&[]));
    }

    #[tokio::test]
    async fn stale_reports_from_a_replaced_viewer_are_dropped() {
        let mut broadcaster = Broadcaster::new(BroadcastConfig::default());
        let (tx, _rx) = mpsc::unbounded_channel();
        broadcaster.viewers.insert(5, Viewer { session: 2, tx });
        let gone = |session| Report {
            viewer_id: 5,
            session,
            event: BroadcastEvent::Gone { viewer_id: 5, reason: "x".into() },
        };
        // The first peer connection for viewer 5 dies after being replaced.
        broadcaster.reports_tx.send(gone(1)).unwrap();
        assert_eq!(broadcaster.poll_event(), None);
        assert_eq!(broadcaster.viewer_count(), 1);
        // The current one dying does count.
        broadcaster.reports_tx.send(gone(2)).unwrap();
        assert!(matches!(broadcaster.poll_event(), Some(BroadcastEvent::Gone { viewer_id: 5, .. })));
        assert_eq!(broadcaster.viewer_count(), 0);
    }
}
