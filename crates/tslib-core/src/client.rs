//! Main client implementation using tsclientlib
//!
//! The client wraps tsclientlib's Connection and provides a higher-level API.
//! Note: The client is not Send/Sync due to tsclientlib limitations.

use crate::config::ClientConfig;
use crate::connection::ConnectionState;
use crate::error::{ConnectionError, Error, Result};
use crate::events::{AudioCodec, Event, EventHandler};
use crate::state::{Channel, ServerState, User};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use futures::{StreamExt, TryStreamExt};
use tsclientlib::prelude::*;
use tsclientlib::{Connection as TsConnection, DisconnectOptions, InMessage, Reason, StreamItem, ClientId, TextMessageTargetMode};
use tsclientlib::MessageTarget as TsMessageTarget;
use tsproto_packets::packets::{AudioData, CodecType, Direction, Flags, OutAudio, OutCommand, PacketType};

/// The main TeamSpeak client
///
/// This client wraps tsclientlib's Connection. Due to tsclientlib's design,
/// the client is not Send/Sync and must be used from a single task.
///
/// Use `process_events()` to poll for and handle server events.
pub struct Client {
    /// The tsclientlib connection
    connection: Option<TsConnection>,
    /// Configuration
    config: ClientConfig,
    /// Current connection state
    state: ConnectionState,
    /// Server state (channels, users, etc.)
    server_state: ServerState,
    /// Event broadcaster
    event_tx: broadcast::Sender<Event>,
    /// Event handlers
    handlers: Vec<Arc<dyn EventHandler>>,
    /// Our client ID on the server
    client_id: Option<u16>,
    /// Current channel ID
    channel_id: Option<u64>,
    /// Audio packet sequence number
    audio_sequence: u16,
}

impl Client {
    /// Connect to a TeamSpeak server
    pub fn connect(config: ClientConfig) -> Result<Self> {
        info!("Connecting to {}", config.address);

        // Create event broadcaster
        let (event_tx, _) = broadcast::channel(256);

        let mut client = Self {
            connection: None,
            config: config.clone(),
            state: ConnectionState::Disconnected,
            server_state: ServerState::default(),
            event_tx,
            handlers: Vec::new(),
            client_id: None,
            channel_id: None,
            audio_sequence: 0,
        };

        // Establish connection
        client.do_connect()?;

        Ok(client)
    }

    /// Perform the actual connection to the server
    fn do_connect(&mut self) -> Result<()> {
        self.state = ConnectionState::Connecting;

        // Build connection options using the new API
        let mut options = TsConnection::build(self.config.address.clone())
            .name(self.config.nickname.clone())
            .identity(self.config.identity.to_ts_identity());

        // Add channel if specified
        if let Some(ref channel) = self.config.channel {
            options = options.channel(channel.clone());
        }

        // Connect
        let con = options
            .connect()
            .map_err(|e| ConnectionError::ConnectFailed(e.to_string()))?;

        self.connection = Some(con);
        self.state = ConnectionState::Connected;

        info!("Connected to {}", self.config.address);

        // Emit connected event
        let _ = self.event_tx.send(Event::Connected {
            server_name: String::new(), // Will be updated from server data
            welcome_message: None,
        });

        Ok(())
    }

    /// Process pending events from the server
    ///
    /// This method must be called regularly to receive server events.
    /// Returns a vector of events that occurred.
    pub async fn process_events(&mut self) -> Result<Vec<Event>> {
        let mut events = Vec::new();
        let mut stream_items = Vec::new();
        let mut disconnected = false;
        let mut error_occurred = false;

        // First, collect all pending stream items
        {
            let con = self
                .connection
                .as_mut()
                .ok_or(ConnectionError::NotConnected)?;

            // Process events with a short timeout
            let timeout = tokio::time::Duration::from_millis(10);

            loop {
                match tokio::time::timeout(timeout, con.events().next()).await {
                    Ok(Some(Ok(item))) => {
                        stream_items.push(item);
                    }
                    Ok(Some(Err(e))) => {
                        warn!("Event stream error: {}", e);
                        error_occurred = true;
                        break;
                    }
                    Ok(None) => {
                        // Stream ended, connection closed
                        disconnected = true;
                        break;
                    }
                    Err(_) => {
                        // Timeout, no more events for now
                        break;
                    }
                }
            }
        }

        // Now process the collected items
        for item in stream_items {
            if let Some(event) = self.process_stream_item(item).await {
                events.push(event);
            }
        }

        if disconnected {
            self.state = ConnectionState::Disconnected;
            let event = Event::Disconnected {
                reason: "Connection closed".to_string(),
            };
            let _ = self.event_tx.send(event.clone());
            events.push(event);
        }

        // Dispatch events to handlers
        for event in &events {
            self.dispatch_event(event.clone()).await;
        }

        Ok(events)
    }

    /// Wait for the initial connection to be established
    ///
    /// This waits until we receive the first BookEvents, indicating
    /// the connection is fully established.
    pub async fn wait_connected(&mut self) -> Result<()> {
        use futures::future;

        let con = self
            .connection
            .as_mut()
            .ok_or(ConnectionError::NotConnected)?;

        let result: Option<std::result::Result<StreamItem, tsclientlib::Error>> = con
            .events()
            .try_filter(|e| future::ready(matches!(e, StreamItem::BookEvents(_))))
            .next()
            .await;

        match result {
            Some(Ok(_)) => {
                // Update server info if available
                if let Ok(state) = con.get_state() {
                    let _ = self.event_tx.send(Event::Connected {
                        server_name: state.server.name.clone(),
                        welcome_message: Some(state.server.welcome_message.clone()),
                    });
                }
                Ok(())
            }
            Some(Err(e)) => Err(ConnectionError::ConnectFailed(e.to_string()).into()),
            None => Err(ConnectionError::ConnectFailed("Connection closed".to_string()).into()),
        }
    }

    /// Process a stream item from tsclientlib
    async fn process_stream_item(&mut self, item: StreamItem) -> Option<Event> {
        match item {
            StreamItem::BookEvents(book_events) => {
                // Process book events (client/channel changes)
                for event in book_events {
                    self.process_book_event(event);
                }
                None
            }
            StreamItem::Audio(audio) => {
                // Audio received - extract data from InAudioBuf
                let audio_data = audio.data().data();
                let (from_id, codec, data) = match audio_data {
                    AudioData::S2C { from, codec, data, .. } => (*from, *codec, data),
                    AudioData::S2CWhisper { from, codec, data, .. } => (*from, *codec, data),
                    _ => return None, // C2S packets should not be received
                };

                let event = Event::AudioReceived {
                    user_id: from_id,
                    codec: codec_type_to_audio_codec(codec),
                    data: data.to_vec(),
                };
                let _ = self.event_tx.send(event.clone());
                Some(event)
            }
            StreamItem::DisconnectedTemporarily(_reason) => {
                self.state = ConnectionState::Reconnecting;
                let event = Event::ConnectionLost {
                    reason: "Temporary disconnect".to_string(),
                };
                let _ = self.event_tx.send(event.clone());
                Some(event)
            }
            StreamItem::MessageEvent(msg) => {
                self.process_message_event(msg)
            }
            _ => None,
        }
    }

    /// Process a book event
    fn process_book_event(&mut self, event: tsclientlib::events::Event) {
        use tsclientlib::events::Event as TsEvent;

        match event {
            TsEvent::PropertyAdded { id, .. } | TsEvent::PropertyChanged { id, .. } => {
                debug!("Property changed: {:?}", id);
            }
            TsEvent::PropertyRemoved { id, .. } => {
                debug!("Property removed: {:?}", id);
            }
            _ => {}
        }
    }

    /// Process an incoming message event
    fn process_message_event(&mut self, msg: InMessage) -> Option<Event> {
        match msg {
            InMessage::TextMessage(text_msg) => {
                // Get the first part of the message (messages can have multiple parts)
                let part = text_msg.iter().next()?;

                // Convert TextMessageTargetMode to our MessageTarget
                let target = match part.target {
                    TextMessageTargetMode::Server => crate::events::MessageTarget::Server,
                    TextMessageTargetMode::Channel => crate::events::MessageTarget::Channel,
                    TextMessageTargetMode::Client => crate::events::MessageTarget::Private,
                    TextMessageTargetMode::Unknown => {
                        warn!("Received message with unknown target mode");
                        return None;
                    }
                };

                let event = Event::TextMessage {
                    sender_id: part.invoker_id.0,
                    sender_name: part.invoker_name.clone(),
                    message: part.message.clone(),
                    target,
                };

                debug!(
                    "Text message from {} ({}): {}",
                    part.invoker_name, part.invoker_id.0, part.message
                );

                let _ = self.event_tx.send(event.clone());
                Some(event)
            }
            InMessage::ClientPoke(poke_msg) => {
                // Get the first part of the poke message
                let part = poke_msg.iter().next()?;

                let event = Event::Poked {
                    poker_id: part.invoker_id.0,
                    poker_name: part.invoker_name.clone(),
                    message: part.message.clone(),
                };

                debug!(
                    "Poked by {} ({}): {}",
                    part.invoker_name, part.invoker_id.0, part.message
                );

                let _ = self.event_tx.send(event.clone());
                Some(event)
            }
            _ => {
                // Other message types we don't handle yet
                debug!("Unhandled message event: {:?}", msg.get_command_name());
                None
            }
        }
    }

    /// Dispatch an event to all handlers
    async fn dispatch_event(&self, event: Event) {
        for handler in &self.handlers {
            handler.on_event(&event).await;
        }
    }

    /// Add an event handler
    pub fn add_handler(&mut self, handler: Arc<dyn EventHandler>) {
        self.handlers.push(handler);
    }

    /// Get the current connection state
    pub fn state(&self) -> ConnectionState {
        self.state.clone()
    }

    /// Check if connected
    pub fn is_connected(&self) -> bool {
        self.state.is_connected()
    }

    /// Get our client ID
    pub fn client_id(&self) -> Option<u16> {
        self.client_id
    }

    /// Get our current channel ID
    pub fn channel_id(&self) -> Option<u64> {
        self.channel_id
    }

    /// Get a snapshot of the server state
    pub fn server_state(&self) -> &ServerState {
        &self.server_state
    }

    /// Get all channels from tsclientlib state
    pub fn channels(&self) -> Vec<Channel> {
        self.server_state.channels.values().cloned().collect()
    }

    /// Get all users from tsclientlib state
    pub fn users(&self) -> Vec<User> {
        self.server_state.users.values().cloned().collect()
    }

    /// Get a specific channel
    pub fn channel(&self, id: u64) -> Option<Channel> {
        self.server_state.channels.get(&id).cloned()
    }

    /// Get a specific user
    pub fn user(&self, id: u16) -> Option<User> {
        self.server_state.users.get(&id).cloned()
    }

    /// Move to a channel
    pub fn move_to_channel(&mut self, channel_id: u64) -> Result<()> {
        self.move_to_channel_with_password(channel_id, None)
    }

    /// Move to a channel with password
    pub fn move_to_channel_with_password(
        &mut self,
        channel_id: u64,
        password: Option<String>,
    ) -> Result<()> {
        let con = self
            .connection
            .as_mut()
            .ok_or(ConnectionError::NotConnected)?;

        // Get our client ID from the state
        let our_client_id = con
            .get_state()
            .map_err(|e| Error::Internal(e.to_string()))?
            .own_client;

        debug!("Moving client {} to channel {}", our_client_id.0, channel_id);

        // Create the clientmove command
        let mut cmd = OutCommand::new(
            Direction::C2S,
            Flags::empty(),
            PacketType::Command,
            "clientmove",
        );
        cmd.write_arg("clid", &our_client_id.0);
        cmd.write_arg("cid", &channel_id);

        // Add password if provided
        if let Some(pwd) = password {
            let encoded_pwd = tsproto_types::crypto::encode_password(pwd.as_bytes());
            cmd.write_arg("cpw", &encoded_pwd);
        }

        // Send the command using OutCommandExt trait
        cmd.send(con)
            .map_err(|e| Error::Internal(e.to_string()))?;

        self.channel_id = Some(channel_id);
        Ok(())
    }

    /// Send a message to the server
    pub fn send_server_message(&mut self, message: impl Into<String>) -> Result<()> {
        let msg = message.into();
        let con = self
            .connection
            .as_mut()
            .ok_or(ConnectionError::NotConnected)?;

        debug!("Sending server message: {}", msg);

        // Get state and create message command, then send it
        con.get_state()
            .map_err(|e| Error::Internal(e.to_string()))?
            .send_message(TsMessageTarget::Server, &msg)
            .send(con)
            .map_err(|e| Error::Internal(e.to_string()))?;

        Ok(())
    }

    /// Send a message to the current channel
    pub fn send_channel_message(&mut self, message: impl Into<String>) -> Result<()> {
        let msg = message.into();
        let con = self
            .connection
            .as_mut()
            .ok_or(ConnectionError::NotConnected)?;

        debug!("Sending channel message: {}", msg);

        // Get state and create message command, then send it
        con.get_state()
            .map_err(|e| Error::Internal(e.to_string()))?
            .send_message(TsMessageTarget::Channel, &msg)
            .send(con)
            .map_err(|e| Error::Internal(e.to_string()))?;

        Ok(())
    }

    /// Send a private message to a user
    pub fn send_private_message(&mut self, user_id: u16, message: impl Into<String>) -> Result<()> {
        let msg = message.into();
        let con = self
            .connection
            .as_mut()
            .ok_or(ConnectionError::NotConnected)?;

        debug!("Sending private message to {}: {}", user_id, msg);

        // Get state and create message command, then send it
        con.get_state()
            .map_err(|e| Error::Internal(e.to_string()))?
            .send_message(TsMessageTarget::Client(ClientId(user_id)), &msg)
            .send(con)
            .map_err(|e| Error::Internal(e.to_string()))?;

        Ok(())
    }

    /// Send audio data
    pub fn send_audio(&mut self, data: &[u8], codec: AudioCodec) -> Result<()> {
        let con = self
            .connection
            .as_mut()
            .ok_or(ConnectionError::NotConnected)?;

        if !con.can_send_audio() {
            return Err(Error::Internal("Cannot send audio".to_string()));
        }

        // Create audio packet using C2S format
        let audio_data = AudioData::C2S {
            id: self.audio_sequence,
            codec: audio_codec_to_codec_type(codec),
            data,
        };
        self.audio_sequence = self.audio_sequence.wrapping_add(1);

        let packet = OutAudio::new(&audio_data);
        con.send_audio(packet)
            .map_err(|e| Error::Internal(e.to_string()))?;

        Ok(())
    }

    /// Subscribe to events
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.event_tx.subscribe()
    }

    /// Disconnect from the server
    pub fn disconnect(&mut self) -> Result<()> {
        self.disconnect_with_reason("Goodbye")
    }

    /// Disconnect from the server with a reason
    pub fn disconnect_with_reason(&mut self, reason: impl Into<String>) -> Result<()> {
        info!("Disconnecting from server");

        if let Some(mut con) = self.connection.take() {
            let options = DisconnectOptions::new()
                .reason(Reason::Clientdisconnect)
                .message(reason.into());

            con.disconnect(options)
                .map_err(|e| ConnectionError::ConnectionLost(e.to_string()))?;
        }

        self.state = ConnectionState::Disconnected;

        let _ = self.event_tx.send(Event::Disconnected {
            reason: "User requested".to_string(),
        });

        Ok(())
    }

    /// Get access to the underlying tsclientlib connection
    pub fn inner(&self) -> Option<&TsConnection> {
        self.connection.as_ref()
    }

    /// Get mutable access to the underlying tsclientlib connection
    pub fn inner_mut(&mut self) -> Option<&mut TsConnection> {
        self.connection.as_mut()
    }
}

/// Convert tsclientlib CodecType to our AudioCodec
fn codec_type_to_audio_codec(codec: CodecType) -> AudioCodec {
    match codec {
        CodecType::SpeexNarrowband => AudioCodec::SpeexNarrowband,
        CodecType::SpeexWideband => AudioCodec::SpeexWideband,
        CodecType::SpeexUltrawideband => AudioCodec::SpeexUltraWideband,
        CodecType::CeltMono => AudioCodec::CeltMono,
        CodecType::OpusVoice => AudioCodec::OpusVoice,
        CodecType::OpusMusic => AudioCodec::OpusMusic,
    }
}

/// Convert our AudioCodec to tsclientlib CodecType
fn audio_codec_to_codec_type(codec: AudioCodec) -> CodecType {
    match codec {
        AudioCodec::SpeexNarrowband => CodecType::SpeexNarrowband,
        AudioCodec::SpeexWideband => CodecType::SpeexWideband,
        AudioCodec::SpeexUltraWideband => CodecType::SpeexUltrawideband,
        AudioCodec::CeltMono => CodecType::CeltMono,
        AudioCodec::OpusVoice => CodecType::OpusVoice,
        AudioCodec::OpusMusic => CodecType::OpusMusic,
    }
}
