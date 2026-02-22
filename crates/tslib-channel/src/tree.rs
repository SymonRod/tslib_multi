//! Channel tree structure

use tslib_core::state::Channel;
use std::collections::HashMap;

/// Channel tree representation
#[derive(Debug, Clone)]
pub struct ChannelTree {
    /// All channels indexed by ID
    channels: HashMap<u64, Channel>,
    /// Root channel IDs
    roots: Vec<u64>,
    /// Children by parent ID
    children: HashMap<u64, Vec<u64>>,
}

impl ChannelTree {
    /// Create an empty channel tree
    pub fn new() -> Self {
        Self {
            channels: HashMap::new(),
            roots: Vec::new(),
            children: HashMap::new(),
        }
    }

    /// Build a tree from a list of channels
    pub fn from_channels(channels: Vec<Channel>) -> Self {
        let mut tree = Self::new();
        for channel in channels {
            tree.add_channel(channel);
        }
        tree.rebuild_structure();
        tree
    }

    /// Add a channel to the tree
    pub fn add_channel(&mut self, channel: Channel) {
        let id = channel.id;
        let parent_id = channel.parent_id;

        self.channels.insert(id, channel);

        // Update structure
        if parent_id == 0 {
            if !self.roots.contains(&id) {
                self.roots.push(id);
            }
        } else {
            self.children
                .entry(parent_id)
                .or_default()
                .push(id);
        }
    }

    /// Remove a channel from the tree
    pub fn remove_channel(&mut self, id: u64) -> Option<Channel> {
        let channel = self.channels.remove(&id)?;

        // Remove from roots
        self.roots.retain(|&x| x != id);

        // Remove from parent's children
        if let Some(children) = self.children.get_mut(&channel.parent_id) {
            children.retain(|&x| x != id);
        }

        // Remove this channel's children entry
        self.children.remove(&id);

        Some(channel)
    }

    /// Update a channel
    pub fn update_channel(&mut self, channel: Channel) {
        let old_parent = self.channels.get(&channel.id).map(|c| c.parent_id);

        // If parent changed, update structure
        if let Some(old_parent_id) = old_parent {
            if old_parent_id != channel.parent_id {
                // Remove from old parent
                if let Some(children) = self.children.get_mut(&old_parent_id) {
                    children.retain(|&x| x != channel.id);
                }

                // Add to new parent
                if channel.parent_id == 0 {
                    self.roots.push(channel.id);
                } else {
                    self.children
                        .entry(channel.parent_id)
                        .or_default()
                        .push(channel.id);
                }
            }
        }

        self.channels.insert(channel.id, channel);
    }

    /// Get a channel by ID
    pub fn get(&self, id: u64) -> Option<&Channel> {
        self.channels.get(&id)
    }

    /// Get root channels
    pub fn roots(&self) -> Vec<&Channel> {
        self.roots
            .iter()
            .filter_map(|id| self.channels.get(id))
            .collect()
    }

    /// Get children of a channel
    pub fn children(&self, parent_id: u64) -> Vec<&Channel> {
        self.children
            .get(&parent_id)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.channels.get(id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get parent of a channel
    pub fn parent(&self, id: u64) -> Option<&Channel> {
        self.channels
            .get(&id)
            .and_then(|c| self.channels.get(&c.parent_id))
    }

    /// Get the path from root to a channel
    pub fn path_to(&self, id: u64) -> Vec<&Channel> {
        let mut path = Vec::new();
        let mut current_id = id;

        while let Some(channel) = self.channels.get(&current_id) {
            path.push(channel);
            if channel.parent_id == 0 {
                break;
            }
            current_id = channel.parent_id;
        }

        path.reverse();
        path
    }

    /// Find a channel by name
    pub fn find_by_name(&self, name: &str) -> Option<&Channel> {
        self.channels.values().find(|c| c.name == name)
    }

    /// Find channels matching a predicate
    pub fn find_all<F>(&self, predicate: F) -> Vec<&Channel>
    where
        F: Fn(&Channel) -> bool,
    {
        self.channels.values().filter(|c| predicate(c)).collect()
    }

    /// Get all channels as a flat list
    pub fn all_channels(&self) -> Vec<&Channel> {
        self.channels.values().collect()
    }

    /// Get channel count
    pub fn len(&self) -> usize {
        self.channels.len()
    }

    /// Check if tree is empty
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    /// Rebuild internal structure
    fn rebuild_structure(&mut self) {
        self.roots.clear();
        self.children.clear();

        for channel in self.channels.values() {
            if channel.parent_id == 0 {
                self.roots.push(channel.id);
            } else {
                self.children
                    .entry(channel.parent_id)
                    .or_default()
                    .push(channel.id);
            }
        }

        // Sort roots by order
        self.roots.sort_by_key(|id| {
            self.channels.get(id).map(|c| c.order).unwrap_or(0)
        });

        // Sort children by order
        for children in self.children.values_mut() {
            children.sort_by_key(|id| {
                self.channels.get(id).map(|c| c.order).unwrap_or(0)
            });
        }
    }

    /// Print the tree structure (for debugging)
    pub fn print_tree(&self) -> String {
        let mut output = String::new();
        for root in &self.roots {
            self.print_subtree(*root, 0, &mut output);
        }
        output
    }

    fn print_subtree(&self, id: u64, depth: usize, output: &mut String) {
        if let Some(channel) = self.channels.get(&id) {
            let indent = "  ".repeat(depth);
            output.push_str(&format!("{}{}\n", indent, channel.name));

            if let Some(children) = self.children.get(&id) {
                for &child_id in children {
                    self.print_subtree(child_id, depth + 1, output);
                }
            }
        }
    }
}

impl Default for ChannelTree {
    fn default() -> Self {
        Self::new()
    }
}
