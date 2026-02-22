//! Main client implementation using tsclientlib
//!
//! The client wraps tsclientlib's Connection and provides a higher-level API.
//! Note: The client is not Send/Sync due to tsclientlib limitations.

use crate::config::ClientConfig;
use crate::connection::ConnectionState;
use crate::error::{ConnectionError, Error, Result};
use crate::events::{AudioCodec, Event, EventHandler};
use crate::state::{Channel, ServerState, User};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use futures::{StreamExt, TryStreamExt};
use tsclientlib::prelude::*;
use tsclientlib::{Connection as TsConnection, DisconnectOptions, InMessage, Reason, StreamItem, ClientId, TextMessageTargetMode};
use tsclientlib::MessageTarget as TsMessageTarget;
use tsclientlib::data::{Client as TsClient, Channel as TsChannel};
use tsclientlib::events::{Event as TsEvent, PropertyId};
use tsproto_types::{ChannelType as TsChannelType, ClientType as TsClientType, Codec as TsCodec};
use tsproto_packets::packets::{AudioData, CodecType, Direction, Flags, OutAudio, OutCommand, PacketType};

/// The main TeamSpeak client
///
/// This client wraps tsclientlib's Connection. Due to tsclientlib's design,
/// the client is not Send/Sync and must be used from a single task.
///
/// Use `process_events()` to poll for and handle server events.
/// Duration after which a user is considered to have stopped talking (ms)
const TALK_TIMEOUT_MS: u128 = 300;

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
    /// Track which users are currently talking (user_id -> last audio timestamp)
    talking_users: HashMap<u16, Instant>,
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
            talking_users: HashMap::new(),
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

        // Add password if specified
        if let Some(ref password) = self.config.password {
            options = options.password(password.clone());
        }

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

        // Check for talk status timeouts
        let talk_stop_events = self.check_talk_timeouts();
        events.extend(talk_stop_events);

        // Dispatch events to handlers
        for event in &events {
            self.dispatch_event(event.clone()).await;
        }

        Ok(events)
    }

    /// Check for users who have stopped talking (no audio received recently)
    fn check_talk_timeouts(&mut self) -> Vec<Event> {
        let now = Instant::now();
        let mut stopped_users = Vec::new();

        // Find users who haven't sent audio recently
        self.talking_users.retain(|&user_id, last_audio| {
            let elapsed = now.duration_since(*last_audio).as_millis();
            if elapsed > TALK_TIMEOUT_MS {
                stopped_users.push(user_id);
                false // Remove from tracking
            } else {
                true // Keep tracking
            }
        });

        // Generate TalkStatusStop events
        stopped_users
            .into_iter()
            .map(|user_id| {
                debug!("User {} stopped talking", user_id);
                let event = Event::TalkStatusStop { user_id };
                let _ = self.event_tx.send(event.clone());
                event
            })
            .collect()
    }

    /// Wait for the initial connection to be established
    ///
    /// This waits until we receive the first BookEvents, indicating
    /// the connection is fully established. It also automatically
    /// subscribes to all channels and synchronizes the server state
    /// (users, channels, etc.).
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
                // Update connection state
                self.state = ConnectionState::Connected;

                // Subscribe to all channels so we can see all users
                self.subscribe_all_channels()?;

                // Synchronize full state from server
                self.sync_state()?;

                // Emit connected event
                let _ = self.event_tx.send(Event::Connected {
                    server_name: self.server_state.server.name.clone(),
                    welcome_message: self.server_state.server.welcome_message.clone(),
                });

                info!(
                    "Connected to {} - {} users, {} channels",
                    self.server_state.server.name,
                    self.server_state.users.len(),
                    self.server_state.channels.len()
                );

                Ok(())
            }
            Some(Err(e)) => Err(ConnectionError::ConnectFailed(e.to_string()).into()),
            None => Err(ConnectionError::ConnectFailed("Connection closed".to_string()).into()),
        }
    }

    /// Subscribe to all channels on the server
    ///
    /// This makes all users visible regardless of which channel they are in.
    pub fn subscribe_all_channels(&mut self) -> Result<()> {
        let con = self
            .connection
            .as_mut()
            .ok_or(ConnectionError::NotConnected)?;

        let cmd = OutCommand::new(
            Direction::C2S,
            Flags::empty(),
            PacketType::Command,
            "channelsubscribeall",
        );

        cmd.send(con)
            .map_err(|e| Error::Internal(e.to_string()))?;

        debug!("Subscribed to all channels");
        Ok(())
    }

    /// Process a stream item from tsclientlib
    async fn process_stream_item(&mut self, item: StreamItem) -> Option<Event> {
        match item {
            StreamItem::BookEvents(book_events) => {
                // Process book events (client/channel changes)
                let mut first_event = None;
                for event in book_events {
                    if let Some(evt) = self.process_book_event(event) {
                        // Send via broadcast channel
                        let _ = self.event_tx.send(evt.clone());
                        if first_event.is_none() {
                            first_event = Some(evt);
                        }
                    }
                }
                first_event
            }
            StreamItem::Audio(audio) => {
                // Audio received - extract data from InAudioBuf
                let audio_data = audio.data().data();
                let (from_id, codec, data) = match audio_data {
                    AudioData::S2C { from, codec, data, .. } => (*from, *codec, data),
                    AudioData::S2CWhisper { from, codec, data, .. } => (*from, *codec, data),
                    _ => return None, // C2S packets should not be received
                };

                // Check if this is a new talk session (user wasn't talking before)
                let was_talking = self.talking_users.contains_key(&from_id);
                self.talking_users.insert(from_id, Instant::now());

                if !was_talking {
                    // Emit TalkStatusStart
                    let start_event = Event::TalkStatusStart { user_id: from_id };
                    let _ = self.event_tx.send(start_event);
                    debug!("User {} started talking", from_id);
                }

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

    /// Process a book event and return any high-level events to emit
    fn process_book_event(&mut self, event: TsEvent) -> Option<Event> {
        match &event {
            TsEvent::PropertyAdded { id, .. } => {
                debug!("Property added: {:?}", id);
                self.handle_property_added(id)
            }
            TsEvent::PropertyChanged { id, old, .. } => {
                debug!("Property changed: {:?}", id);
                self.handle_property_changed(id, old)
            }
            TsEvent::PropertyRemoved { id, old, .. } => {
                debug!("Property removed: {:?}", id);
                self.handle_property_removed(id, old)
            }
            _ => None,
        }
    }

    /// Handle a property being added (new client, channel, etc.)
    fn handle_property_added(&mut self, id: &PropertyId) -> Option<Event> {
        let con = self.connection.as_ref()?;
        let state = con.get_state().ok()?;

        match id {
            PropertyId::Client(client_id) => {
                // New client connected
                if let Some(ts_client) = state.clients.get(client_id) {
                    let user = ts_client_to_user(ts_client);
                    self.server_state.users.insert(user.id, user.clone());
                    info!("User joined: {} ({})", user.nickname, user.id);
                    return Some(Event::UserJoined { user });
                }
            }
            PropertyId::Channel(channel_id) => {
                // New channel created
                if let Some(ts_channel) = state.channels.get(channel_id) {
                    let channel = ts_channel_to_channel(ts_channel);
                    self.server_state.channels.insert(channel.id, channel.clone());
                    info!("Channel created: {} ({})", channel.name, channel.id);
                    return Some(Event::ChannelCreated { channel });
                }
            }
            _ => {}
        }
        None
    }

    /// Handle a property being changed
    fn handle_property_changed(&mut self, id: &PropertyId, _old: &tsclientlib::events::PropertyValue) -> Option<Event> {
        let con = self.connection.as_ref()?;
        let state = con.get_state().ok()?;

        match id {
            PropertyId::ClientChannel(client_id) => {
                // Client moved to a different channel
                if let Some(ts_client) = state.clients.get(client_id) {
                    let user = ts_client_to_user(ts_client);
                    let from_channel = self.server_state.users
                        .get(&user.id)
                        .map(|u| u.channel_id)
                        .unwrap_or(0);

                    self.server_state.users.insert(user.id, user.clone());

                    if from_channel != user.channel_id {
                        info!("User {} moved from channel {} to {}", user.nickname, from_channel, user.channel_id);
                        return Some(Event::UserMoved {
                            user,
                            from_channel,
                            to_channel: ts_client.channel.0,
                        });
                    }
                }
            }
            PropertyId::ClientName(client_id) |
            PropertyId::ClientInputMuted(client_id) |
            PropertyId::ClientOutputMuted(client_id) |
            PropertyId::ClientAwayMessage(client_id) |
            PropertyId::ClientTalkPower(client_id) |
            PropertyId::ClientTalkPowerGranted(client_id) |
            PropertyId::ClientIsRecording(client_id) => {
                // Client property changed - update our state
                if let Some(ts_client) = state.clients.get(client_id) {
                    let user = ts_client_to_user(ts_client);
                    self.server_state.users.insert(user.id, user.clone());
                    return Some(Event::UserUpdated { user });
                }
            }
            PropertyId::ChannelName(channel_id) |
            PropertyId::ChannelTopic(channel_id) |
            PropertyId::ChannelCodec(channel_id) |
            PropertyId::ChannelMaxClients(channel_id) |
            PropertyId::ChannelNeededTalkPower(channel_id) => {
                // Channel property changed
                if let Some(ts_channel) = state.channels.get(channel_id) {
                    let channel = ts_channel_to_channel(ts_channel);
                    self.server_state.channels.insert(channel.id, channel.clone());
                    return Some(Event::ChannelEdited { channel });
                }
            }
            _ => {}
        }
        None
    }

    /// Handle a property being removed (client left, channel deleted, etc.)
    fn handle_property_removed(&mut self, id: &PropertyId, _old: &tsclientlib::events::PropertyValue) -> Option<Event> {
        match id {
            PropertyId::Client(client_id) => {
                // Client disconnected
                if let Some(user) = self.server_state.users.remove(&client_id.0) {
                    info!("User left: {} ({})", user.nickname, user.id);
                    return Some(Event::UserLeft {
                        user,
                        reason: "Disconnected".to_string(),
                    });
                }
            }
            PropertyId::Channel(channel_id) => {
                // Channel deleted
                if self.server_state.channels.remove(&channel_id.0).is_some() {
                    info!("Channel deleted: {}", channel_id.0);
                    return Some(Event::ChannelDeleted {
                        channel_id: channel_id.0,
                    });
                }
            }
            _ => {}
        }
        None
    }

    /// Synchronize the full state from tsclientlib
    /// Call this after connection to populate initial state
    pub fn sync_state(&mut self) -> Result<()> {
        let con = self.connection.as_ref().ok_or(ConnectionError::NotConnected)?;
        let state = con.get_state().map_err(|e| Error::Internal(e.to_string()))?;

        // Clear existing state
        self.server_state.users.clear();
        self.server_state.channels.clear();

        // Sync server info
        self.server_state.server.name = state.server.name.clone();
        self.server_state.server.welcome_message = Some(state.server.welcome_message.clone());
        self.server_state.server.platform = state.server.platform.clone();
        self.server_state.server.version = state.server.version.clone();
        self.server_state.server.max_clients = state.server.max_clients as u32;

        // Sync all channels
        for (_, ts_channel) in &state.channels {
            let channel = ts_channel_to_channel(ts_channel);
            self.server_state.channels.insert(channel.id, channel);
        }

        // Sync all clients
        for (_, ts_client) in &state.clients {
            let user = ts_client_to_user(ts_client);
            self.server_state.users.insert(user.id, user);
        }

        // Update our client ID
        self.client_id = Some(state.own_client.0);

        // Update our channel ID
        if let Some(our_client) = state.clients.get(&state.own_client) {
            self.channel_id = Some(our_client.channel.0);
        }

        info!(
            "State synced: {} users, {} channels",
            self.server_state.users.len(),
            self.server_state.channels.len()
        );

        Ok(())
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

/// Convert tsclientlib Client to our User type
fn ts_client_to_user(client: &TsClient) -> User {
    // Convert UID bytes to base64 string
    let uid = client.uid.as_ref()
        .map(|u| base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &u.0))
        .unwrap_or_default();

    // Check client type
    let client_type = if matches!(client.client_type, TsClientType::Query { .. }) { 1 } else { 0 };

    User {
        id: client.id.0,
        uid,
        database_id: client.database_id.0,
        channel_id: client.channel.0,
        nickname: client.name.clone(),
        client_type,
        is_talking: false, // Tracked separately via audio
        is_input_muted: client.input_muted,
        is_output_muted: client.output_muted,
        has_input_hardware: client.input_hardware_enabled,
        has_output_hardware: client.output_hardware_enabled,
        is_away: client.away_message.is_some(),
        away_message: client.away_message.clone(),
        is_recording: client.is_recording,
        is_priority_speaker: client.is_priority_speaker,
        is_channel_commander: client.is_channel_commander,
        talk_power: client.talk_power,
        is_talker: client.talk_power_granted,
        server_groups: client.server_groups.iter().map(|g| g.0).collect(),
        channel_group: client.channel_group.0,
        platform: client.optional_data.as_ref().map(|o| o.platform.clone()).unwrap_or_default(),
        version: client.optional_data.as_ref().map(|o| o.version.clone()).unwrap_or_default(),
        country: None, // Not directly available in tsclientlib
        description: None,
        avatar_id: None,
        icon_id: 0,
        idle_time: 0,
        connected_time: 0,
    }
}

/// Convert tsclientlib Channel to our Channel type
fn ts_channel_to_channel(channel: &TsChannel) -> Channel {
    let (is_permanent, is_semi_permanent) = match channel.channel_type {
        TsChannelType::Permanent => (true, false),
        TsChannelType::SemiPermanent => (false, true),
        TsChannelType::Temporary => (false, false),
    };

    // Extract description from optional data
    let description = channel.optional_data.as_ref().map(|o| o.description.clone());

    // Convert MaxClients enum to i32 (-1 for unlimited)
    let max_clients = channel.max_clients
        .map(|m| match m {
            tsproto_types::MaxClients::Unlimited => -1,
            tsproto_types::MaxClients::Inherited => -1,
            tsproto_types::MaxClients::Limited(n) => n as i32,
        })
        .unwrap_or(-1);

    let max_family_clients = channel.max_family_clients
        .map(|m| match m {
            tsproto_types::MaxClients::Unlimited => -1,
            tsproto_types::MaxClients::Inherited => -1,
            tsproto_types::MaxClients::Limited(n) => n as i32,
        })
        .unwrap_or(-1);

    Channel {
        id: channel.id.0,
        parent_id: channel.parent.0,
        name: channel.name.clone(),
        topic: channel.topic.clone(),
        description,
        order: channel.order.0 as i32,
        is_permanent,
        is_semi_permanent,
        is_default: channel.is_default.unwrap_or(false),
        has_password: channel.has_password.unwrap_or(false),
        codec: ts_codec_to_u8(channel.codec),
        codec_quality: channel.codec_quality.unwrap_or(7),
        max_clients,
        max_family_clients,
        needed_talk_power: channel.needed_talk_power.unwrap_or(0),
        icon_id: 0,
        is_subscribed: channel.subscribed,
    }
}

/// Convert tsclientlib Codec to u8
fn ts_codec_to_u8(codec: TsCodec) -> u8 {
    match codec {
        TsCodec::SpeexNarrowband => 0,
        TsCodec::SpeexWideband => 1,
        TsCodec::SpeexUltrawideband => 2,
        TsCodec::CeltMono => 3,
        TsCodec::OpusVoice => 4,
        TsCodec::OpusMusic => 5,
    }
}
