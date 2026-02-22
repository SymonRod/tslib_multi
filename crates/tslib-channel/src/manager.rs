//! Channel management

use crate::tree::ChannelTree;
use tslib_core::state::Channel;
use tslib_core::error::Result;
use std::sync::Arc;
use tokio::sync::RwLock;

/// High-level channel manager
pub struct ChannelManager {
    /// Channel tree
    tree: Arc<RwLock<ChannelTree>>,
    /// Current channel ID
    current_channel: Arc<RwLock<Option<u64>>>,
}

impl ChannelManager {
    /// Create a new channel manager
    pub fn new() -> Self {
        Self {
            tree: Arc::new(RwLock::new(ChannelTree::new())),
            current_channel: Arc::new(RwLock::new(None)),
        }
    }

    /// Get the channel tree
    pub async fn tree(&self) -> ChannelTree {
        self.tree.read().await.clone()
    }

    /// Set the channel tree
    pub async fn set_tree(&self, tree: ChannelTree) {
        *self.tree.write().await = tree;
    }

    /// Add a channel
    pub async fn add_channel(&self, channel: Channel) {
        self.tree.write().await.add_channel(channel);
    }

    /// Remove a channel
    pub async fn remove_channel(&self, id: u64) -> Option<Channel> {
        self.tree.write().await.remove_channel(id)
    }

    /// Update a channel
    pub async fn update_channel(&self, channel: Channel) {
        self.tree.write().await.update_channel(channel);
    }

    /// Get a channel by ID
    pub async fn get_channel(&self, id: u64) -> Option<Channel> {
        self.tree.read().await.get(id).cloned()
    }

    /// Get current channel
    pub async fn current_channel(&self) -> Option<u64> {
        *self.current_channel.read().await
    }

    /// Set current channel
    pub async fn set_current_channel(&self, id: u64) {
        *self.current_channel.write().await = Some(id);
    }

    /// Find channel by name
    pub async fn find_by_name(&self, name: &str) -> Option<Channel> {
        self.tree.read().await.find_by_name(name).cloned()
    }

    /// Get root channels
    pub async fn root_channels(&self) -> Vec<Channel> {
        self.tree.read().await.roots().into_iter().cloned().collect()
    }

    /// Get children of a channel
    pub async fn children(&self, parent_id: u64) -> Vec<Channel> {
        self.tree.read().await.children(parent_id).into_iter().cloned().collect()
    }

    /// Get path to a channel
    pub async fn path_to(&self, id: u64) -> Vec<Channel> {
        self.tree.read().await.path_to(id).into_iter().cloned().collect()
    }

    /// Get all channels
    pub async fn all_channels(&self) -> Vec<Channel> {
        self.tree.read().await.all_channels().into_iter().cloned().collect()
    }

    /// Get joinable channels (not full, no password or we know it)
    pub async fn joinable_channels(&self) -> Vec<Channel> {
        self.tree
            .read()
            .await
            .find_all(|c| !c.has_password && !c.is_spacer())
            .into_iter()
            .cloned()
            .collect()
    }

    /// Print tree structure
    pub async fn print_tree(&self) -> String {
        self.tree.read().await.print_tree()
    }
}

impl Default for ChannelManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Channel creation builder
#[derive(Debug, Clone, Default)]
pub struct ChannelBuilder {
    name: Option<String>,
    parent_id: u64,
    topic: Option<String>,
    description: Option<String>,
    password: Option<String>,
    codec: u8,
    codec_quality: u8,
    max_clients: i32,
    is_permanent: bool,
    is_semi_permanent: bool,
    order: i32,
}

impl ChannelBuilder {
    /// Create a new channel builder
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            codec: 4,          // Opus Voice
            codec_quality: 6,  // Medium quality
            max_clients: -1,   // Unlimited
            ..Default::default()
        }
    }

    /// Set parent channel
    pub fn parent(mut self, parent_id: u64) -> Self {
        self.parent_id = parent_id;
        self
    }

    /// Set topic
    pub fn topic(mut self, topic: impl Into<String>) -> Self {
        self.topic = Some(topic.into());
        self
    }

    /// Set description
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set password
    pub fn password(mut self, password: impl Into<String>) -> Self {
        self.password = Some(password.into());
        self
    }

    /// Set codec (4 = Opus Voice, 5 = Opus Music)
    pub fn codec(mut self, codec: u8) -> Self {
        self.codec = codec;
        self
    }

    /// Set codec quality (1-10)
    pub fn codec_quality(mut self, quality: u8) -> Self {
        self.codec_quality = quality.clamp(1, 10);
        self
    }

    /// Set max clients (-1 for unlimited)
    pub fn max_clients(mut self, max: i32) -> Self {
        self.max_clients = max;
        self
    }

    /// Make channel permanent
    pub fn permanent(mut self) -> Self {
        self.is_permanent = true;
        self.is_semi_permanent = false;
        self
    }

    /// Make channel semi-permanent
    pub fn semi_permanent(mut self) -> Self {
        self.is_semi_permanent = true;
        self.is_permanent = false;
        self
    }

    /// Make channel temporary (default)
    pub fn temporary(mut self) -> Self {
        self.is_permanent = false;
        self.is_semi_permanent = false;
        self
    }

    /// Set channel order
    pub fn order(mut self, order: i32) -> Self {
        self.order = order;
        self
    }

    /// Build the channel (for local use, not server creation)
    pub fn build(self) -> Option<Channel> {
        Some(Channel {
            id: 0, // Will be assigned by server
            parent_id: self.parent_id,
            name: self.name?,
            topic: self.topic,
            description: self.description,
            order: self.order,
            is_permanent: self.is_permanent,
            is_semi_permanent: self.is_semi_permanent,
            is_default: false,
            has_password: self.password.is_some(),
            codec: self.codec,
            codec_quality: self.codec_quality,
            max_clients: self.max_clients,
            max_family_clients: -1,
            needed_talk_power: 0,
            icon_id: 0,
            is_subscribed: false,
        })
    }
}
