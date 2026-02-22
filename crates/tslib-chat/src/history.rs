//! Chat history management

use crate::message::{ChatMessage, MessageTarget};
use std::collections::VecDeque;

/// Chat history storage
pub struct ChatHistory {
    /// Maximum messages to keep per conversation
    max_messages: usize,
    /// Server messages
    server_messages: VecDeque<ChatMessage>,
    /// Channel messages (by channel ID)
    channel_messages: std::collections::HashMap<u64, VecDeque<ChatMessage>>,
    /// Private messages (by user ID)
    private_messages: std::collections::HashMap<u16, VecDeque<ChatMessage>>,
}

impl ChatHistory {
    /// Create a new chat history
    pub fn new(max_messages: usize) -> Self {
        Self {
            max_messages,
            server_messages: VecDeque::with_capacity(max_messages),
            channel_messages: std::collections::HashMap::new(),
            private_messages: std::collections::HashMap::new(),
        }
    }

    /// Add a message to history
    pub fn add(&mut self, message: ChatMessage) {
        let queue = match message.target {
            MessageTarget::Server => &mut self.server_messages,
            MessageTarget::Channel(id) => self
                .channel_messages
                .entry(id)
                .or_insert_with(|| VecDeque::with_capacity(self.max_messages)),
            MessageTarget::Private(id) => self
                .private_messages
                .entry(id)
                .or_insert_with(|| VecDeque::with_capacity(self.max_messages)),
        };

        if queue.len() >= self.max_messages {
            queue.pop_front();
        }
        queue.push_back(message);
    }

    /// Get server messages
    pub fn server_messages(&self) -> impl Iterator<Item = &ChatMessage> {
        self.server_messages.iter()
    }

    /// Get channel messages
    pub fn channel_messages(&self, channel_id: u64) -> impl Iterator<Item = &ChatMessage> {
        self.channel_messages
            .get(&channel_id)
            .map(|q| q.iter())
            .into_iter()
            .flatten()
    }

    /// Get private messages with a user
    pub fn private_messages(&self, user_id: u16) -> impl Iterator<Item = &ChatMessage> {
        self.private_messages
            .get(&user_id)
            .map(|q| q.iter())
            .into_iter()
            .flatten()
    }

    /// Get all messages (across all conversations)
    pub fn all_messages(&self) -> impl Iterator<Item = &ChatMessage> {
        self.server_messages
            .iter()
            .chain(self.channel_messages.values().flat_map(|q| q.iter()))
            .chain(self.private_messages.values().flat_map(|q| q.iter()))
    }

    /// Search messages by content
    pub fn search(&self, query: &str) -> Vec<&ChatMessage> {
        let query = query.to_lowercase();
        self.all_messages()
            .filter(|m| m.content.to_lowercase().contains(&query))
            .collect()
    }

    /// Get messages from a specific sender
    pub fn from_sender(&self, sender_id: u16) -> Vec<&ChatMessage> {
        self.all_messages()
            .filter(|m| m.sender_id == sender_id)
            .collect()
    }

    /// Clear all history
    pub fn clear(&mut self) {
        self.server_messages.clear();
        self.channel_messages.clear();
        self.private_messages.clear();
    }

    /// Clear history for a specific channel
    pub fn clear_channel(&mut self, channel_id: u64) {
        self.channel_messages.remove(&channel_id);
    }

    /// Clear history for a specific user
    pub fn clear_user(&mut self, user_id: u16) {
        self.private_messages.remove(&user_id);
    }

    /// Get total message count
    pub fn total_count(&self) -> usize {
        self.server_messages.len()
            + self.channel_messages.values().map(|q| q.len()).sum::<usize>()
            + self.private_messages.values().map(|q| q.len()).sum::<usize>()
    }
}

impl Default for ChatHistory {
    fn default() -> Self {
        Self::new(1000)
    }
}
