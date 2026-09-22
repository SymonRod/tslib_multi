use super::*;
use tsclientlib::events::{ExtraInfo, PropertyValue};
use tsproto_packets::packets::OutPacket;

fn client() -> Client {
    let config = ClientConfig::builder()
        .address("localhost:9987")
        .identity(crate::Identity::create().unwrap())
        .build()
        .unwrap();
    let mut client = Client::connect(config).unwrap();
    // The connection has not been polled: these tests never open a session.
    client.connection = None;
    client
}

fn notification(raw: &str) -> StreamItem {
    let header = OutPacket::new_with_dir(Direction::S2C, Flags::empty(), PacketType::Command);
    StreamItem::MessageEvent(InMessage::new(&header.header(), raw.as_bytes()).unwrap())
}

fn snapshot(event: &Event) -> &[Stream] {
    match event {
        Event::StreamsChanged { streams } => streams,
        _ => panic!("Expected stream snapshot, got {event:?}"),
    }
}

#[tokio::test]
async fn notifications_return_and_broadcast_one_complete_snapshot() {
    let mut client = client();
    let mut receiver = client.subscribe();
    for raw in [
        "notifystreamstarted clid=2 id=a name=First audio=1",
        "notifystreaminfo clid=2 id=a viewer=3|id=b name=Second",
        "notifystreamupdated clid=2 id=a name=Renamed",
        "notifystreamstopped clid=2 id=a",
    ] {
        let events = client.process_stream_item(notification(raw)).await;
        assert_eq!(events.len(), 1);
        assert_eq!(snapshot(&events[0]), client.streams());
        assert_eq!(snapshot(&receiver.try_recv().unwrap()), client.streams());
        assert!(receiver.try_recv().is_err());
        assert!(client
            .process_stream_item(notification(raw))
            .await
            .is_empty());
        assert!(receiver.try_recv().is_err());
    }
}

#[tokio::test]
async fn owner_removal_cleans_all_streams_even_without_cached_user() {
    let mut client = client();
    client
        .process_stream_item(notification(
            "notifystreamstarted clid=2 id=a|id=b|clid=3 id=a",
        ))
        .await;
    assert!(client.server_state.users.is_empty());
    let mut receiver = client.subscribe();
    let events = client
        .process_stream_item(StreamItem::BookEvents(vec![TsEvent::PropertyRemoved {
            id: PropertyId::Client(ClientId(2)),
            // The cleanup uses the key, not the removed book value.
            old: PropertyValue::String(String::new()),
            invoker: None,
            extra: ExtraInfo::default(),
        }]))
        .await;
    assert_eq!(events.len(), 1);
    assert_eq!(snapshot(&events[0]).len(), 1);
    assert_eq!(snapshot(&events[0])[0].owner_id, 3);
    assert_eq!(snapshot(&receiver.try_recv().unwrap()), client.streams());
    assert!(receiver.try_recv().is_err());
}

#[tokio::test]
async fn temporary_disconnect_clears_registry_and_reconnect_accepts_streams() {
    let mut client = client();
    client
        .process_stream_item(notification("notifystreamstarted clid=2 id=a"))
        .await;
    let mut receiver = client.subscribe();
    let events = client
        .process_stream_item(StreamItem::DisconnectedTemporarily(
            tsclientlib::TemporaryDisconnectReason::Serverstop,
        ))
        .await;
    assert_eq!(events.len(), 2);
    assert!(snapshot(&events[0]).is_empty());
    assert!(snapshot(&receiver.try_recv().unwrap()).is_empty());
    assert!(matches!(
        receiver.try_recv().unwrap(),
        Event::ConnectionLost { .. }
    ));
    assert!(receiver.try_recv().is_err());
    assert!(client.streams().is_empty());
    client
        .process_stream_item(StreamItem::BookEvents(Vec::new()))
        .await;
    let events = client
        .process_stream_item(notification("notifystreamstarted clid=2 id=b"))
        .await;
    assert_eq!(snapshot(&events[0])[0].id, "b");
}

#[tokio::test]
async fn explicit_disconnect_clears_once_and_ignores_queued_metadata() {
    let mut client = client();
    client
        .process_stream_item(notification("notifystreamstarted clid=2 id=a"))
        .await;
    let mut receiver = client.subscribe();
    client.disconnect().unwrap();
    assert!(client.streams().is_empty());
    assert!(snapshot(&receiver.try_recv().unwrap()).is_empty());
    assert!(matches!(
        receiver.try_recv().unwrap(),
        Event::Disconnected { .. }
    ));
    assert!(client
        .process_stream_item(notification("notifystreaminfo clid=2 id=a"))
        .await
        .is_empty());
    assert!(client.clear_streams().is_none());
    assert!(receiver.try_recv().is_err());
}

#[tokio::test]
async fn join_response_carries_the_offer_and_reaches_subscribers() {
    let mut client = client();
    let mut receiver = client.subscribe();
    let events = client
        .process_stream_item(notification(
            r"notifyrespondjoinstreamrequest clid=2 id=a msg decision=1 offer=v=0\r\na=x\r\n",
        ))
        .await;
    assert_eq!(events.len(), 1);
    let Event::StreamJoinResponse { owner_id, stream_id, decision, message, offer } = &events[0]
    else {
        panic!("Expected a join response, got {:?}", events[0]);
    };
    assert_eq!(*owner_id, 2);
    assert_eq!(stream_id, "a");
    assert_eq!(*decision, 1);
    assert_eq!(message.as_deref(), Some(""));
    // tsproto already unescaped the SDP: the app must not unescape it again.
    assert_eq!(offer.as_deref(), Some("v=0\r\na=x\r\n"));
    assert!(matches!(
        receiver.try_recv().unwrap(),
        Event::StreamJoinResponse { .. }
    ));
    assert!(receiver.try_recv().is_err());
}

#[tokio::test]
async fn a_refusal_has_no_offer() {
    let mut client = client();
    let events = client
        .process_stream_item(notification(
            "notifyrespondjoinstreamrequest clid=2 id=a decision=0",
        ))
        .await;
    let Event::StreamJoinResponse { decision, offer, message, .. } = &events[0] else {
        panic!("Expected a join response, got {:?}", events[0]);
    };
    assert_eq!(*decision, 0);
    assert_eq!(*offer, None);
    assert_eq!(*message, None);
}

#[tokio::test]
async fn signaling_json_is_forwarded_untouched() {
    let mut client = client();
    let mut receiver = client.subscribe();
    // Not valid JSON: the server relays without inspecting, so the app decides.
    let events = client
        .process_stream_item(notification("notifystreamsignaling clid=2 id=a json=not-json"))
        .await;
    let Event::StreamSignaling { owner_id, stream_id, json } = &events[0] else {
        panic!("Expected signaling, got {:?}", events[0]);
    };
    assert_eq!(*owner_id, 2);
    assert_eq!(stream_id, "a");
    assert_eq!(json, "not-json");
    assert!(matches!(
        receiver.try_recv().unwrap(),
        Event::StreamSignaling { .. }
    ));
}

#[tokio::test]
async fn viewer_commands_need_a_connection() {
    let mut client = client();
    assert!(client.join_stream(2, "a").is_err());
    assert!(client.leave_stream(2, "a").is_err());
    assert!(client.send_stream_signaling(2, "a", "{}").is_err());
    assert!(client.request_stream_info(2, None).is_err());
}

#[tokio::test]
async fn join_requests_from_viewers_become_events() {
    let mut client = client();
    let mut receiver = client.subscribe();
    for (raw, remove) in [
        ("notifyjoinstreamrequest clid=5 id=a msg is_remove=0", false),
        ("notifyjoinstreamrequest clid=5 id=a msg is_remove=1", true),
    ] {
        let events = client.process_stream_item(notification(raw)).await;
        let Event::StreamJoinRequest { viewer_id, stream_id, is_remove } = &events[0] else {
            panic!("Expected a join request, got {:?}", events[0]);
        };
        assert_eq!(*viewer_id, 5);
        assert_eq!(stream_id, "a");
        assert_eq!(*is_remove, remove);
        assert!(matches!(receiver.try_recv().unwrap(), Event::StreamJoinRequest { .. }));
    }
}

#[tokio::test]
async fn own_streams_are_the_ones_we_own() {
    let mut client = client();
    assert!(client.own_streams().is_empty());
    client.client_id = Some(7);
    client
        .process_stream_item(notification("notifystreamstarted clid=7 id=mine|clid=2 id=theirs"))
        .await;
    let own = client.own_streams();
    assert_eq!(own.len(), 1);
    assert_eq!(own[0].id, "mine");
}

#[tokio::test]
async fn broadcaster_commands_need_a_connection() {
    let mut client = client();
    assert!(client.setup_stream(&StreamSetup::new("Test", 1_500_000)).is_err());
    assert!(client.rename_stream("a", "Renamed").is_err());
    assert!(client.stop_stream("a").is_err());
    assert!(client.accept_stream_viewer(5, "a", "v=0").is_err());
    assert!(client.refuse_stream_viewer(5, "a").is_err());
    assert!(client.remove_stream_viewer(5, "a").is_err());
}
