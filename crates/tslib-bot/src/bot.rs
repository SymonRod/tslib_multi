//! Main bot implementation

use crate::command::{Command, CommandContext, CommandHandler, CommandRegistry, FnHandler};
use crate::config::BotConfig;
use crate::error::{BotError, Result};
use crate::plugin::{Plugin, PluginManager};
use std::future::Future;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use tslib_core::{Client, ClientConfig, Event, EventHandler};

/// TeamSpeak bot
pub struct Bot {
    /// Configuration
    config: BotConfig,
    /// Core client
    client: Option<Client>,
    /// Command registry
    commands: Arc<RwLock<CommandRegistry>>,
    /// Plugin manager
    plugins: Arc<RwLock<PluginManager>>,
    /// Is running
    running: Arc<RwLock<bool>>,
}

impl Bot {
    /// Create a new bot
    pub async fn new(config: BotConfig) -> Result<Self> {
        let commands = Arc::new(RwLock::new(CommandRegistry::new(&config.command_prefix)));

        let mut bot = Self {
            config,
            client: None,
            commands,
            plugins: Arc::new(RwLock::new(PluginManager::new())),
            running: Arc::new(RwLock::new(false)),
        };

        // Register built-in commands
        bot.register_builtin_commands().await;

        Ok(bot)
    }

    /// Connect to the server
    pub async fn connect(&mut self) -> Result<()> {
        let core_config = ClientConfig::builder()
            .address(&self.config.address)
            .identity(self.config.identity.clone())
            .nickname(&self.config.nickname)
            .build()
            .map_err(|e| BotError::Core(e))?;

        let client = Client::connect(core_config)?;
        self.client = Some(client);

        info!("Bot connected to {}", self.config.address);
        Ok(())
    }

    /// Disconnect from the server
    pub async fn disconnect(&mut self) -> Result<()> {
        if let Some(mut client) = self.client.take() {
            client.disconnect()?;
        }
        Ok(())
    }

    /// Register a command
    pub async fn command<F, Fut>(&self, name: &str, handler: F)
    where
        F: Fn(CommandContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let cmd = Command::new(name, FnHandler::new(handler));
        self.commands.write().await.register(cmd);
    }

    /// Register a command with full options
    pub async fn register_command(&self, command: Command) {
        self.commands.write().await.register(command);
    }

    /// Load a plugin
    pub async fn load_plugin(&self, plugin: Box<dyn Plugin>) -> Result<()> {
        self.plugins.write().await.load(plugin, self).await
    }

    /// Run the bot (blocking)
    pub async fn run(&mut self) -> Result<()> {
        if self.client.is_none() {
            self.connect().await?;
        }

        *self.running.write().await = true;

        let client = self.client.as_ref().unwrap();
        let mut events = client.subscribe();
        let commands = self.commands.clone();
        let config = self.config.clone();

        info!("Bot is running. Press Ctrl+C to stop.");

        while *self.running.read().await {
            tokio::select! {
                event = events.recv() => {
                    match event {
                        Ok(Event::TextMessage { sender_id, sender_name, message, target }) => {
                            Self::handle_message(
                                &commands,
                                &config,
                                sender_id,
                                sender_name,
                                None,
                                message,
                                matches!(target, tslib_core::events::MessageTarget::Private),
                            ).await;
                        }
                        Ok(Event::Disconnected { reason }) => {
                            warn!("Disconnected: {}", reason);
                            if config.auto_reconnect {
                                info!("Reconnecting in {:?}...", config.reconnect_delay);
                                tokio::time::sleep(config.reconnect_delay).await;
                                // TODO: Reconnect
                            }
                        }
                        Ok(_) => {}
                        Err(e) => {
                            debug!("Event error: {}", e);
                        }
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    info!("Shutting down...");
                    break;
                }
            }
        }

        self.disconnect().await?;
        Ok(())
    }

    /// Stop the bot
    pub async fn stop(&self) {
        *self.running.write().await = false;
    }

    /// Check if user is an owner
    pub fn is_owner(&self, uid: &str) -> bool {
        self.config.owners.iter().any(|o| o == uid)
    }

    /// Get the core client
    pub fn client(&self) -> Option<&Client> {
        self.client.as_ref()
    }

    async fn handle_message(
        commands: &Arc<RwLock<CommandRegistry>>,
        config: &BotConfig,
        sender_id: u16,
        sender_name: String,
        sender_uid: Option<String>,
        message: String,
        is_private: bool,
    ) {
        let registry = commands.read().await;

        if let Some((cmd_name, args)) = registry.parse(&message) {
            let cmd_name_owned = cmd_name.to_string();
            if let Some(command) = registry.get(&cmd_name_owned) {
                // Check owner-only
                if command.owner_only {
                    if let Some(ref uid) = sender_uid {
                        if !config.owners.contains(uid) {
                            debug!("User {} tried to use owner-only command {}", sender_name, cmd_name_owned);
                            return;
                        }
                    } else {
                        return;
                    }
                }

                let ctx = CommandContext::new(
                    sender_id,
                    sender_name.clone(),
                    sender_uid,
                    cmd_name_owned.clone(),
                    args,
                    message,
                    is_private,
                    |_msg| Box::pin(async { Ok(()) }), // TODO: Actual reply
                );

                // Check can_use
                if !command.handler.can_use(&ctx) {
                    debug!("User {} cannot use command {}", sender_name, cmd_name_owned);
                    return;
                }

                // Execute command
                match command.handler.handle(ctx).await {
                    Ok(()) => {
                        debug!("Command {} executed successfully", cmd_name_owned);
                    }
                    Err(e) => {
                        error!("Command {} failed: {}", cmd_name_owned, e);
                    }
                }
            }
        }
    }

    async fn register_builtin_commands(&mut self) {
        // Help command
        let commands = self.commands.clone();
        self.command("help", move |ctx| {
            let commands = commands.clone();
            async move {
                let registry = commands.read().await;
                let cmds = registry.visible_commands();

                let mut help_text = String::from("Available commands:\n");
                for cmd in cmds {
                    help_text.push_str(&format!("  {} - {}\n",
                        cmd.name,
                        cmd.help.as_deref().unwrap_or("No description")
                    ));
                }

                ctx.reply(help_text).await
            }
        }).await;

        // Ping command
        self.command("ping", |ctx| async move {
            ctx.reply("Pong!").await
        }).await;

        // Version command
        self.command("version", |ctx| async move {
            ctx.reply(format!("tslib-bot v{}", env!("CARGO_PKG_VERSION"))).await
        }).await;
    }
}
