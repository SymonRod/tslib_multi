//! TeamSpeak 6 screen sharing, broadcaster side.
//!
//! [`tslib_core`] carries the signaling — `setupstream`, join requests, the
//! offer and the relayed WebRTC messages. This crate does the media: one
//! ffmpeg VP8 encode ([`VideoEncoder`]) fanned out to one WebRTC peer
//! connection per viewer ([`Broadcaster`]).
//!
//! The two are glued by the caller, because `tslib_core::Client` is not
//! `Send` and stays on its own task:
//!
//! ```ignore
//! // Event::StreamJoinRequest   -> broadcaster.add_viewer / remove_viewer
//! // Event::StreamSignaling     -> broadcaster.handle_signaling
//! // broadcaster.poll_event()   -> BroadcastEvent::Offer -> client.accept_stream_viewer
//! ```
//!
//! Only P2P mode exists (TeamSpeak has not released the SFU), so every viewer
//! costs the broadcaster one full upload of the video. There is no TURN
//! fallback either: the broadcaster must be reachable over UDP, which inside
//! Docker means host networking.
//!
//! The wire protocol is documented in `TS6_Droid/docs/screenshare-protocol.md`.

mod broadcaster;
mod encoder;

pub use broadcaster::{BroadcastConfig, BroadcastEvent, Broadcaster};
pub use encoder::{EncoderConfig, VideoEncoder, VideoInput};

/// Errors from this crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// ffmpeg or the input command could not be started.
    #[error("cannot start {program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    /// WebRTC setup failed.
    #[error("webrtc: {0}")]
    WebRtc(#[from] webrtc::error::Error),
    /// The peer connection failed or was closed by the viewer.
    #[error("peer connection {0}")]
    PeerConnection(String),
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;
