//! # tslib-audio
//!
//! Audio module for tslib providing:
//! - Microphone capture
//! - Speaker playback
//! - Opus encoding/decoding
//! - Audio mixing
//! - Echo cancellation (planned)
//!
//! ## Example
//!
//! ```rust,no_run
//! use tslib_audio::{AudioManager, AudioConfig};
//!
//! # async fn example() -> anyhow::Result<()> {
//! let config = AudioConfig::default();
//! let mut audio = AudioManager::new(config)?;
//!
//! // Start capture
//! audio.start_capture()?;
//!
//! // Get encoded audio for transmission
//! while let Some(packet) = audio.get_encoded_packet().await {
//!     // Send packet to server...
//! }
//! # Ok(())
//! # }
//! ```

pub mod capture;
pub mod codec;
pub mod config;
pub mod error;
pub mod manager;
pub mod mixer;
pub mod playback;

// Re-exports
pub use codec::{Decoder, Encoder, OpusCodec};
pub use config::AudioConfig;
pub use error::{AudioError, Result};
pub use manager::AudioManager;
