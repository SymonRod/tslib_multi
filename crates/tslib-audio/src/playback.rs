//! Audio playback (speaker output)

use crate::config::AudioConfig;
use crate::error::{AudioError, Result};
use std::collections::HashMap;
use tokio::sync::mpsc;

/// Audio playback device
pub struct PlaybackDevice {
    config: AudioConfig,
    /// Is currently playing
    is_active: bool,
    /// Volume level
    volume: f32,
}

impl PlaybackDevice {
    /// Create a new playback device
    pub fn new(config: AudioConfig) -> Result<Self> {
        Ok(Self {
            config,
            is_active: false,
            volume: 1.0,
        })
    }

    /// List available playback devices
    pub fn list_devices() -> Result<Vec<super::capture::DeviceInfo>> {
        // TODO: Use cpal to enumerate devices
        Ok(vec![super::capture::DeviceInfo {
            name: "Default".to_string(),
            is_default: true,
        }])
    }

    /// Start playback
    pub fn start(&mut self) -> Result<()> {
        if self.is_active {
            return Ok(());
        }

        self.is_active = true;

        // TODO: Start actual audio playback thread using cpal

        Ok(())
    }

    /// Stop playback
    pub fn stop(&mut self) -> Result<()> {
        self.is_active = false;
        Ok(())
    }

    /// Check if playing
    pub fn is_active(&self) -> bool {
        self.is_active
    }

    /// Set output volume (0.0-2.0)
    pub fn set_volume(&mut self, volume: f32) -> Result<()> {
        if volume < 0.0 || volume > 2.0 {
            return Err(AudioError::InvalidConfig("Volume must be 0.0-2.0".into()));
        }
        self.volume = volume;
        Ok(())
    }

    /// Get current volume
    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// Queue audio for playback from a specific user
    pub fn queue_audio(&mut self, user_id: u16, samples: &[i16]) -> Result<()> {
        // TODO: Add to playback buffer
        Ok(())
    }
}

/// Audio buffer for a single user
pub struct UserAudioBuffer {
    /// User ID
    user_id: u16,
    /// Ring buffer of samples
    buffer: Vec<i16>,
    /// Write position
    write_pos: usize,
    /// Read position
    read_pos: usize,
    /// Volume modifier for this user
    volume: f32,
    /// Is user muted
    is_muted: bool,
    /// Last activity timestamp
    last_activity: std::time::Instant,
}

impl UserAudioBuffer {
    /// Create a new buffer for a user
    pub fn new(user_id: u16, capacity: usize) -> Self {
        Self {
            user_id,
            buffer: vec![0; capacity],
            write_pos: 0,
            read_pos: 0,
            volume: 1.0,
            is_muted: false,
            last_activity: std::time::Instant::now(),
        }
    }

    /// Write samples to the buffer
    pub fn write(&mut self, samples: &[i16]) -> Result<()> {
        for &sample in samples {
            self.buffer[self.write_pos] = sample;
            self.write_pos = (self.write_pos + 1) % self.buffer.len();
        }
        self.last_activity = std::time::Instant::now();
        Ok(())
    }

    /// Read samples from the buffer
    pub fn read(&mut self, output: &mut [i16]) -> usize {
        let mut count = 0;
        for out in output.iter_mut() {
            if self.read_pos == self.write_pos {
                break;
            }
            let sample = self.buffer[self.read_pos];
            *out = if self.is_muted {
                0
            } else {
                (sample as f32 * self.volume) as i16
            };
            self.read_pos = (self.read_pos + 1) % self.buffer.len();
            count += 1;
        }
        count
    }

    /// Available samples in buffer
    pub fn available(&self) -> usize {
        if self.write_pos >= self.read_pos {
            self.write_pos - self.read_pos
        } else {
            self.buffer.len() - self.read_pos + self.write_pos
        }
    }

    /// Set volume for this user
    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 2.0);
    }

    /// Mute/unmute this user
    pub fn set_muted(&mut self, muted: bool) {
        self.is_muted = muted;
    }

    /// Check if buffer is stale (no recent activity)
    pub fn is_stale(&self, timeout: std::time::Duration) -> bool {
        self.last_activity.elapsed() > timeout
    }
}
