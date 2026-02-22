//! Audio capture (microphone input)

use crate::config::AudioConfig;
use crate::error::{AudioError, Result};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Audio capture device
pub struct CaptureDevice {
    config: AudioConfig,
    /// Sender for captured audio frames
    frame_tx: Option<mpsc::Sender<AudioFrame>>,
    /// Is currently capturing
    is_active: bool,
}

/// A captured audio frame
#[derive(Debug, Clone)]
pub struct AudioFrame {
    /// PCM samples (16-bit signed)
    pub samples: Vec<i16>,
    /// Timestamp in microseconds
    pub timestamp_us: u64,
    /// Voice activity detected
    pub voice_activity: bool,
}

impl CaptureDevice {
    /// Create a new capture device
    pub fn new(config: AudioConfig) -> Result<Self> {
        Ok(Self {
            config,
            frame_tx: None,
            is_active: false,
        })
    }

    /// List available capture devices
    pub fn list_devices() -> Result<Vec<DeviceInfo>> {
        // TODO: Use cpal to enumerate devices
        Ok(vec![DeviceInfo {
            name: "Default".to_string(),
            is_default: true,
        }])
    }

    /// Start capturing audio
    pub fn start(&mut self, frame_tx: mpsc::Sender<AudioFrame>) -> Result<()> {
        if self.is_active {
            return Ok(());
        }

        self.frame_tx = Some(frame_tx);
        self.is_active = true;

        // TODO: Start actual audio capture thread using cpal
        // For now, just mark as active

        Ok(())
    }

    /// Stop capturing audio
    pub fn stop(&mut self) -> Result<()> {
        self.is_active = false;
        self.frame_tx = None;
        Ok(())
    }

    /// Check if capturing
    pub fn is_active(&self) -> bool {
        self.is_active
    }

    /// Set input volume (0.0-2.0)
    pub fn set_volume(&mut self, volume: f32) -> Result<()> {
        if volume < 0.0 || volume > 2.0 {
            return Err(AudioError::InvalidConfig("Volume must be 0.0-2.0".into()));
        }
        // TODO: Apply volume
        Ok(())
    }
}

/// Device information
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// Device name
    pub name: String,
    /// Is this the system default device
    pub is_default: bool,
}

/// Voice Activity Detector
pub struct VoiceActivityDetector {
    threshold: f32,
    /// Smoothing factor
    smoothing: f32,
    /// Current energy level
    current_energy: f32,
}

impl VoiceActivityDetector {
    /// Create a new VAD
    pub fn new(threshold: f32) -> Self {
        Self {
            threshold,
            smoothing: 0.95,
            current_energy: 0.0,
        }
    }

    /// Process samples and detect voice activity
    pub fn process(&mut self, samples: &[i16]) -> bool {
        // Calculate RMS energy
        let sum: f64 = samples.iter().map(|&s| (s as f64).powi(2)).sum();
        let rms = (sum / samples.len() as f64).sqrt() as f32;

        // Normalize to 0-1 range
        let energy = rms / 32768.0;

        // Smooth the energy
        self.current_energy = self.smoothing * self.current_energy + (1.0 - self.smoothing) * energy;

        self.current_energy > self.threshold
    }

    /// Set the threshold
    pub fn set_threshold(&mut self, threshold: f32) {
        self.threshold = threshold.clamp(0.0, 1.0);
    }

    /// Get current energy level
    pub fn energy(&self) -> f32 {
        self.current_energy
    }
}
