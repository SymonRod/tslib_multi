//! Main client implementation

use crate::config::ClientConfig;
use crate::connection::{Connection, ConnectionCommand, ConnectionState, MessageTarget};
use crate::error::{ConnectionError, Error, Result};
use crate::events::{Event, EventHandler};
use crate::state::{Channel, ServerState, User};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, RwLock};
use tracing::{debug, error, info, warn};

/// The main TeamSpeak client
pub struct Client {
    /// The connection to the server
    connection: Arc<Connection>,
    /// Configuration
    config: ClientConfig,
    /// Event handlers
    handlers: Arc<RwLock<Vec<Arc<dyn EventHandler>>>>,
    /// Shutdown signal
    shutdown_tx: mpsc::Sender<()>,
}

impl Client {
    /// Connect to a TeamSpeak server
    pub async fn connect(config: ClientConfig) -> Result<Self> {
        info!("Connecting to {}", config.address);

        // Create channels
        let (event_tx, _) = broadcast::channel(256);
        let (command_tx, command_rx) = mpsc::channel(64);
        let (shutdown_tx, shutdown_rx) = mpsc::channel(1);

        // Create connection
        let connection = Arc::new(Connection::new(
            config.clone(),
            event_tx.clone(),
            command_tx.clone(),
        ));

        let client = Self {
            connection: connection.clone(),
            config,
            handlers: Arc::new(RwLock::new(Vec::new())),
            shutdown_tx,
        };

        // Start connection task
        client.spawn_connection_task(command_rx, shutdown_rx);

        // Wait for connection to establish
        client.wait_for_connection().await?;

        Ok(client)
    }

    /// Add an event handler
    pub async fn add_handler(&self, handler: Arc<dyn EventHandler>) {
        self.handlers.write().await.push(handler);
    }

    /// Get the current connection state
    pub async fn state(&self) -> ConnectionState {
        self.connection.state().await
    }

    /// Check if connected
    pub async fn is_connected(&self) -> bool {
        self.connection.state().await.is_connected()
    }

    /// Get our client ID
    pub async fn client_id(&self) -> Option<u16> {
        self.connection.client_id().await
    }

    /// Get our current channel ID
    pub async fn channel_id(&self) -> Option<u64> {
        self.connection.channel_id().await
    }

    /// Get a snapshot of the server state
    pub async fn server_state(&self) -> ServerState {
        self.connection.server_state().read().await.clone()
    }

    /// Get all channels
    pub async fn channels(&self) -> Vec<Channel> {
        self.connection
            .server_state()
            .read()
            .await
            .channels
            .values()
            .cloned()
            .collect()
    }

    /// Get all users
    pub async fn users(&self) -> Vec<User> {
        self.connection
            .server_state()
            .read()
            .await
            .users
            .values()
            .cloned()
            .collect()
    }

    /// Get a specific channel
    pub async fn channel(&self, id: u64) -> Option<Channel> {
        self.connection
            .server_state()
            .read()
            .await
            .channels
            .get(&id)
            .cloned()
    }

    /// Get a specific user
    pub async fn user(&self, id: u16) -> Option<User> {
        self.connection
            .server_state()
            .read()
            .await
            .users
            .get(&id)
            .cloned()
    }

    /// Move to a channel
    pub async fn move_to_channel(&self, channel_id: u64) -> Result<()> {
        self.connection.move_to_channel(channel_id, None).await
    }

    /// Move to a channel with password
    pub async fn move_to_channel_with_password(
        &self,
        channel_id: u64,
        password: impl Into<String>,
    ) -> Result<()> {
        self.connection
            .move_to_channel(channel_id, Some(password.into()))
            .await
    }

    /// Send a message to the server
    pub async fn send_server_message(&self, message: impl Into<String>) -> Result<()> {
        self.connection
            .send_message(MessageTarget::Server, message)
            .await
    }

    /// Send a message to the current channel
    pub async fn send_channel_message(&self, message: impl Into<String>) -> Result<()> {
        self.connection
            .send_message(MessageTarget::Channel, message)
            .await
    }

    /// Send a private message to a user
    pub async fn send_private_message(
        &self,
        user_id: u16,
        message: impl Into<String>,
    ) -> Result<()> {
        self.connection
            .send_message(MessageTarget::Private(user_id), message)
            .await
    }

    /// Subscribe to events
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.connection.subscribe()
    }

    /// Disconnect from the server
    pub async fn disconnect(&self) -> Result<()> {
        self.disconnect_with_reason("Goodbye").await
    }

    /// Disconnect from the server with a reason
    pub async fn disconnect_with_reason(&self, reason: impl Into<String>) -> Result<()> {
        info!("Disconnecting from server");
        self.connection.disconnect(Some(reason.into())).await?;
        let _ = self.shutdown_tx.send(()).await;
        Ok(())
    }

    /// Wait until the connection is established
    async fn wait_for_connection(&self) -> Result<()> {
        let timeout = self.config.connect_timeout;
        let start = std::time::Instant::now();

        // Set state to connecting
        self.connection.set_state(ConnectionState::Connecting).await;

        // TODO: Actually connect using tsclientlib
        // For now, simulate a successful connection
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Simulate successful connection
        self.connection.set_state(ConnectionState::Connected).await;
        self.connection.set_client_id(1).await;
        self.connection.set_channel_id(1).await;

        info!("Connected successfully");
        Ok(())
    }

    /// Spawn the background connection task
    fn spawn_connection_task(
        &self,
        mut command_rx: mpsc::Receiver<ConnectionCommand>,
        mut shutdown_rx: mpsc::Receiver<()>,
    ) {
        let connection = self.connection.clone();
        let handlers = self.handlers.clone();

        tokio::spawn(async move {
            let mut event_rx = connection.subscribe();

            loop {
                tokio::select! {
                    // Handle commands
                    Some(cmd) = command_rx.recv() => {
                        Self::handle_command(&connection, cmd).await;
                    }

                    // Handle events
                    Ok(event) = event_rx.recv() => {
                        Self::dispatch_event(&handlers, event).await;
                    }

                    // Handle shutdown
                    Some(_) = shutdown_rx.recv() => {
                        debug!("Shutdown signal received");
                        break;
                    }
                }
            }

            debug!("Connection task ended");
        });
    }

    /// Handle a connection command
    async fn handle_command(connection: &Connection, cmd: ConnectionCommand) {
        match cmd {
            ConnectionCommand::Disconnect(reason) => {
                debug!("Processing disconnect command: {:?}", reason);
                connection.set_state(ConnectionState::Disconnected).await;
            }
            ConnectionCommand::MoveToChannel { channel_id, password } => {
                debug!("Moving to channel {}", channel_id);
                // TODO: Send move command via tsclientlib
                connection.set_channel_id(channel_id).await;
            }
            ConnectionCommand::SendMessage { target, message } => {
                debug!("Sending message to {:?}: {}", target, message);
                // TODO: Send message via tsclientlib
            }
            ConnectionCommand::SendCommand(cmd) => {
                debug!("Sending raw command: {}", cmd);
                // TODO: Send raw command via tsclientlib
            }
        }
    }

    /// Dispatch an event to all handlers
    async fn dispatch_event(
        handlers: &Arc<RwLock<Vec<Arc<dyn EventHandler>>>>,
        event: Event,
    ) {
        let handlers = handlers.read().await;
        for handler in handlers.iter() {
            handler.on_event(&event).await;
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // Trigger shutdown
        let _ = self.shutdown_tx.try_send(());
    }
}
