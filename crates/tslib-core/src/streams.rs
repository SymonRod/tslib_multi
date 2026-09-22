//! Metadata for streams advertised on the TeamSpeak command channel.

use std::collections::HashMap;
use tsclientlib::InMessage;

/// An advertised stream. Missing metadata remains unknown until received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stream {
    pub owner_id: u16,
    pub id: String,
    pub name: Option<String>,
    pub stream_type: Option<u32>,
    pub access: Option<u32>,
    pub mode: Option<u32>,
    pub bitrate: Option<u32>,
    pub viewer_limit: Option<u32>,
    pub viewer_count: Option<u32>,
    pub audio: Option<bool>,
}

/// Source type: a whole screen. The desktop client also uses 1 (camera) and
/// 3 (window).
pub const STREAM_TYPE_SCREEN: u32 = 2;
/// Accessibility: anyone who can see us may ask to join.
pub const STREAM_ACCESS_PUBLIC: u32 = 1;
/// Transport mode. The SFU (2) is not released yet.
pub(crate) const STREAM_MODE_P2P: u32 = 1;
/// `ScreenshareLeaveReason` values the broadcaster sends.
pub(crate) const STREAM_REASON_LEFT: u32 = 1;
pub(crate) const STREAM_REASON_KICKED: u32 = 4;

/// What to publish with [`Client::setup_stream`](crate::Client::setup_stream).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamSetup {
    pub name: String,
    pub stream_type: u32,
    pub accessibility: u32,
    /// Requested bitrate in bit/s; the server reports its own figure back.
    pub bitrate: u32,
    /// 0 means unlimited.
    pub viewer_limit: u32,
    /// Whether the WebRTC session carries an audio track.
    pub audio: bool,
}

impl StreamSetup {
    /// A public, video-only screen share.
    pub fn new(name: impl Into<String>, bitrate: u32) -> Self {
        Self {
            name: name.into(),
            stream_type: STREAM_TYPE_SCREEN,
            accessibility: STREAM_ACCESS_PUBLIC,
            bitrate,
            viewer_limit: 0,
            audio: false,
        }
    }
}

#[derive(Default)]
pub(crate) struct StreamRegistry {
    streams: HashMap<(u16, String), Stream>,
}

impl StreamRegistry {
    pub(crate) fn apply(&mut self, msg: &InMessage) -> bool {
        let mut changed = false;
        match msg {
            InMessage::StreamStarted(parts) => {
                for part in parts.iter() {
                    changed |= self.merge(Stream {
                        owner_id: part.client_id.0,
                        id: part.stream_id.clone(),
                        name: part.name.clone(),
                        stream_type: part.stream_type,
                        access: part.access,
                        mode: part.mode,
                        bitrate: part.bitrate,
                        viewer_limit: part.viewer_limit,
                        viewer_count: None,
                        audio: part.audio,
                    });
                }
            }
            InMessage::StreamInfo(parts) => {
                for part in parts.iter() {
                    changed |= self.merge(Stream {
                        owner_id: part.client_id.0,
                        id: part.stream_id.clone(),
                        name: part.name.clone(),
                        stream_type: part.stream_type,
                        access: part.accessibility,
                        mode: part.mode,
                        bitrate: part.bitrate,
                        viewer_limit: part.viewer_limit,
                        viewer_count: part.viewer_count,
                        audio: part.audio,
                    });
                }
            }
            InMessage::StreamUpdated(parts) => {
                for part in parts.iter() {
                    changed |= self.merge(Stream {
                        owner_id: part.client_id.0,
                        id: part.stream_id.clone(),
                        name: part.name.clone(),
                        stream_type: part.stream_type,
                        access: part.accessibility.or(part.access),
                        mode: part.mode,
                        bitrate: part.bitrate,
                        viewer_limit: part.viewer_limit,
                        viewer_count: part.viewer_count,
                        audio: part.audio,
                    });
                }
            }
            InMessage::StreamStopped(parts) => {
                for part in parts.iter() {
                    changed |= self
                        .streams
                        .remove(&(part.client_id.0, part.stream_id.clone()))
                        .is_some();
                }
            }
            _ => {}
        }
        changed
    }

    fn merge(&mut self, mut stream: Stream) -> bool {
        let key = (stream.owner_id, stream.id.clone());
        if let Some(previous) = self.streams.get(&key) {
            stream.name = stream.name.or_else(|| previous.name.clone());
            stream.stream_type = stream.stream_type.or(previous.stream_type);
            stream.access = stream.access.or(previous.access);
            stream.mode = stream.mode.or(previous.mode);
            stream.bitrate = stream.bitrate.or(previous.bitrate);
            stream.viewer_limit = stream.viewer_limit.or(previous.viewer_limit);
            stream.viewer_count = stream.viewer_count.or(previous.viewer_count);
            stream.audio = stream.audio.or(previous.audio);
            if &stream == previous {
                return false;
            }
        }
        self.streams.insert(key, stream);
        true
    }

    pub(crate) fn remove_owner(&mut self, owner_id: u16) -> bool {
        let previous_len = self.streams.len();
        self.streams.retain(|(owner, _), _| *owner != owner_id);
        self.streams.len() != previous_len
    }

    pub(crate) fn clear(&mut self) -> bool {
        let changed = !self.streams.is_empty();
        self.streams.clear();
        changed
    }

    pub(crate) fn list(&self) -> Vec<Stream> {
        let mut streams: Vec<_> = self.streams.values().cloned().collect();
        streams.sort_unstable_by(|a, b| a.owner_id.cmp(&b.owner_id).then_with(|| a.id.cmp(&b.id)));
        streams
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tsproto_packets::packets::{Direction, Flags, OutPacket, PacketType};

    fn parse(raw: &str) -> InMessage {
        let header = OutPacket::new_with_dir(Direction::S2C, Flags::empty(), PacketType::Command);
        InMessage::new(&header.header(), raw.as_bytes()).unwrap()
    }

    fn empty(owner_id: u16, id: &str) -> Stream {
        Stream {
            owner_id,
            id: id.into(),
            name: None,
            stream_type: None,
            access: None,
            mode: None,
            bitrate: None,
            viewer_limit: None,
            viewer_count: None,
            audio: None,
        }
    }

    #[test]
    fn lifecycle() {
        let mut registry = StreamRegistry::default();
        let start = parse(
            r"notifystreamstarted clid=13 id=s name=Wayland\sSource type=3 access=1 mode=1 bitrate=33920 viewer_limit=5 audio=1",
        );
        assert!(registry.apply(&start));
        let mut expected = Stream {
            name: Some("Wayland Source".into()),
            stream_type: Some(3),
            access: Some(1),
            mode: Some(1),
            bitrate: Some(33920),
            viewer_limit: Some(5),
            audio: Some(true),
            ..empty(13, "s")
        };
        assert_eq!(registry.list(), vec![expected.clone()]);
        assert!(!registry.apply(&start));
        let info = parse("notifystreaminfo clid=13 id=s viewer=2 accessibility=2");
        assert!(registry.apply(&info));
        expected.viewer_count = Some(2);
        expected.access = Some(2);
        assert_eq!(registry.list(), vec![expected.clone()]);
        assert!(!registry.apply(&info));
        let update = parse("notifystreamupdated clid=13 id=s name=Renamed access=3");
        assert!(registry.apply(&update));
        expected.name = Some("Renamed".into());
        expected.access = Some(3);
        assert_eq!(registry.list(), vec![expected]);
        assert!(!registry.apply(&update));
        let stop = parse("notifystreamstopped clid=13 id=s reason=99");
        assert!(registry.apply(&stop));
        assert!(registry.list().is_empty());
        assert!(!registry.apply(&stop));
    }

    #[test]
    fn info_and_update_before_started() {
        for command in ["notifystreaminfo", "notifystreamupdated"] {
            let mut registry = StreamRegistry::default();
            assert!(registry.apply(&parse(&format!("{command} clid=1 id=s"))));
            assert_eq!(registry.list(), vec![empty(1, "s")]);
            assert!(!registry.apply(&parse(&format!("{command} clid=1 id=s"))));
            assert!(registry.apply(&parse(&format!(
                "{command} clid=1 id=s name=Early type=99 accessibility=98 mode=97 bitrate=96 viewer_limit=95 viewer=94 audio=1"
            ))));
            let expected = Stream {
                name: Some("Early".into()),
                stream_type: Some(99),
                access: Some(98),
                mode: Some(97),
                bitrate: Some(96),
                viewer_limit: Some(95),
                viewer_count: Some(94),
                audio: Some(true),
                ..empty(1, "s")
            };
            assert_eq!(registry.list(), vec![expected.clone()]);
            assert!(!registry.apply(&parse("notifystreamstarted clid=1 id=s")));
            assert_eq!(registry.list(), vec![expected]);
        }
    }

    #[test]
    fn partial_updates_preserve_absent_fields_and_accept_empty_zero_false() {
        let mut registry = StreamRegistry::default();
        registry.apply(&parse("notifystreaminfo clid=1 id=s name=Full type=1 accessibility=1 mode=1 bitrate=1 viewer_limit=1 viewer=1 audio=1"));
        for field in [
            "name=",
            "type=0",
            "accessibility=0 access=9",
            "mode=0",
            "bitrate=0",
            "viewer_limit=0",
            "viewer=0",
            "audio=0",
        ] {
            let before = registry.list().remove(0);
            let mut expected = before.clone();
            match field {
                "name=" => expected.name = Some(String::new()),
                "type=0" => expected.stream_type = Some(0),
                "accessibility=0 access=9" => expected.access = Some(0),
                "mode=0" => expected.mode = Some(0),
                "bitrate=0" => expected.bitrate = Some(0),
                "viewer_limit=0" => expected.viewer_limit = Some(0),
                "viewer=0" => expected.viewer_count = Some(0),
                "audio=0" => expected.audio = Some(false),
                _ => unreachable!(),
            }
            let update = parse(&format!("notifystreamupdated clid=1 id=s {field}"));
            assert!(registry.apply(&update), "{field}");
            assert_eq!(registry.list(), vec![expected], "{field}");
            assert!(!registry.apply(&update), "{field}");
        }
        assert!(!registry.apply(&parse(
            "notifystreamupdated clid=1 id=s return_code=ignored"
        )));
    }

    #[test]
    fn multipart_inheritance_for_every_notification() {
        for command in [
            "notifystreamstarted",
            "notifystreaminfo",
            "notifystreamupdated",
        ] {
            let mut registry = StreamRegistry::default();
            let message = parse(&format!(
                "{command} clid=2 id=z name=Shared audio=1|id=a|clid=1 id=z name=Other audio=0"
            ));
            assert!(registry.apply(&message));
            assert_eq!(
                registry.list(),
                vec![
                    Stream {
                        name: Some("Other".into()),
                        audio: Some(false),
                        ..empty(1, "z")
                    },
                    Stream {
                        name: Some("Shared".into()),
                        audio: Some(true),
                        ..empty(2, "a")
                    },
                    Stream {
                        name: Some("Shared".into()),
                        audio: Some(true),
                        ..empty(2, "z")
                    },
                ]
            );
            assert!(!registry.apply(&message));
            assert!(registry.apply(&parse("notifystreamstopped clid=2 id=missing|id=a|id=z")));
            assert_eq!(
                registry.list(),
                vec![Stream {
                    name: Some("Other".into()),
                    audio: Some(false),
                    ..empty(1, "z")
                },]
            );
            assert!(!registry.apply(&parse("notifystreamstopped clid=2 id=z")));
        }
    }

    #[test]
    fn owner_cleanup_clear_and_snapshot_independence() {
        let mut registry = StreamRegistry::default();
        assert!(!registry.clear());
        assert!(!registry.remove_owner(1));
        registry.apply(&parse("notifystreamstarted clid=2 id=z|id=a|clid=1 id=z"));
        let snapshot = registry.list();
        assert_eq!(snapshot, vec![empty(1, "z"), empty(2, "a"), empty(2, "z")]);
        assert!(!registry.remove_owner(99));
        assert!(registry.remove_owner(2));
        assert!(!registry.remove_owner(2));
        assert_eq!(registry.list(), vec![empty(1, "z")]);
        assert!(registry.clear());
        assert!(!registry.clear());
        assert!(registry.list().is_empty());
        assert_eq!(snapshot.len(), 3);
    }

    #[test]
    fn unrelated_notifications_do_not_change_metadata() {
        let mut registry = StreamRegistry::default();
        registry.apply(&parse("notifystreaminfo clid=1 id=s viewer=5"));
        let before = registry.list();
        for command in [
            "notifystreamclientjoined clid=1 id=s",
            "notifystreamclientleft clid=1 id=s reason=1",
            "notifystreamsignaling clid=1 id=s json=opaque",
        ] {
            assert!(!registry.apply(&parse(command)));
            assert_eq!(registry.list(), before);
        }
    }
}
