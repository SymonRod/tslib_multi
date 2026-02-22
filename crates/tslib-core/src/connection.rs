//! Connection management

use crate::config::ClientConfig;
use crate::error::{ConnectionError, Result};
use crate::events::Event;
use crate::state::ServerState;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, RwLock};

/// Connection state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Not connected to any server
    Disconnected,
    /// Currently connecting
    Connecting,
    /// Connected and authenticated
    Connected,
    /// Connection established, receiving initial data
    Initializing,
    /// Connection lost, attempting to reconnect
    Reconnecting,
}

impl ConnectionState {
    /// Check if the connection is active
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected | Self::Initializing)
    }

    /// Check if a connection attempt is in progress
    pub fn is_connecting(&self) -> bool {
        matches!(self, Self::Connecting | Self::Reconnecting)
    }
}

/// A connection to a TeamSpeak 3 server
pub struct Connection {
    /// Current connection state
    state: Arc<RwLock<ConnectionState>>,
    /// Server state (channels, users, etc.)
    server_state: Arc<RwLock<ServerState>>,
    /// Configuration used for this connection
    config: ClientConfig,
    /// Event broadcaster
    event_tx: broadcast::Sender<Event>,
    /// Command sender for internal operations
    command_tx: mpsc::Sender<ConnectionCommand>,
    /// Our client ID on the server
    client_id: Arc<RwLock<Option<u16>>>,
    /// Current channel ID
    channel_id: Arc<RwLock<Option<u64>>>,
}

/// Internal commands for connection management
#[derive(Debug)]
pub(crate) enum ConnectionCommand {
    /// Send a command to the server
    SendCommand(String),
    /// Disconnect from the server
    Disconnect(Option<String>),
    /// Move to a channel
    MoveToChannel { channel_id: u64, password: Option<String> },
    /// Send a text message
    SendMessage { target: MessageTarget, message: String },
}

/// Message target
#[derive(Debug, Clone)]
pub enum MessageTarget {
    /// Send to the entire server
    Server,
    /// Send to the current channel
    Channel,
    /// Send to a specific user
    Private(u16),
}

impl Connection {
    /// Create a new connection (internal use)
    pub(crate) fn new(
        config: ClientConfig,
        event_tx: broadcast::Sender<Event>,
        command_tx: mpsc::Sender<ConnectionCommand>,
    ) -> Self {
        Self {
            state: Arc::new(RwLock::new(ConnectionState::Disconnected)),
            server_state: Arc::new(RwLock::new(ServerState::default())),
            config,
            event_tx,
            command_tx,
            client_id: Arc::new(RwLock::new(None)),
            channel_id: Arc::new(RwLock::new(None)),
        }
    }

    /// Get the current connection state
    pub async fn state(&self) -> ConnectionState {
        *self.state.read().await
    }

    /// Get a reference to the server state
    pub fn server_state(&self) -> &Arc<RwLock<ServerState>> {
        &self.server_state
    }

    /// Get our client ID on the server
    pub async fn client_id(&self) -> Option<u16> {
        *self.client_id.read().await
    }

    /// Get our current channel ID
    pub async fn channel_id(&self) -> Option<u64> {
        *self.channel_id.read().await
    }

    /// Subscribe to events
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.event_tx.subscribe()
    }

    /// Move to a channel
    pub async fn move_to_channel(
        &self,
        channel_id: u64,
        password: Option<String>,
    ) -> Result<()> {
        if !self.state().await.is_connected() {
            return Err(ConnectionError::NotConnected.into());
        }

        self.command_tx
            .send(ConnectionCommand::MoveToChannel { channel_id, password })
            .await
            .map_err(|_| ConnectionError::ConnectionLost("Command channel closed".into()))?;

        Ok(())
    }

    /// Send a text message
    pub async fn send_message(
        &self,
        target: MessageTarget,
        message: impl Into<String>,
    ) -> Result<()> {
        if !self.state().await.is_connected() {
            return Err(ConnectionError::NotConnected.into());
        }

        self.command_tx
            .send(ConnectionCommand::SendMessage {
                target,
                message: message.into(),
            })
            .await
            .map_err(|_| ConnectionError::ConnectionLost("Command channel closed".into()))?;

        Ok(())
    }

    /// Disconnect from the server
    pub async fn disconnect(&self, reason: Option<String>) -> Result<()> {
        self.command_tx
            .send(ConnectionCommand::Disconnect(reason))
            .await
            .map_err(|_| ConnectionError::NotConnected)?;

        Ok(())
    }

    // Internal state management

    pub(crate) async fn set_state(&self, new_state: ConnectionState) {
        let mut state = self.state.write().await;
        let old_state = *state;
        *state = new_state;

        // Emit state change event
        let _ = self.event_tx.send(Event::ConnectionStateChanged {
            old_state,
            new_state,
        });
    }

    pub(crate) async fn set_client_id(&self, id: u16) {
        *self.client_id.write().await = Some(id);
    }

    pub(crate) async fn set_channel_id(&self, id: u64) {
        *self.channel_id.write().await = Some(id);
    }
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("config", &self.config.address)
            .finish_non_exhaustive()
    }
}
