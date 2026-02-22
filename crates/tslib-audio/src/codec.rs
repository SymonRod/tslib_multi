//! Audio codec handling (Opus)

use crate::config::{AudioConfig, OpusApplication};
use crate::error::{AudioError, Result};

/// Audio encoder trait
pub trait Encoder: Send {
    /// Encode PCM samples to compressed data
    fn encode(&mut self, pcm: &[i16], output: &mut [u8]) -> Result<usize>;

    /// Reset encoder state
    fn reset(&mut self) -> Result<()>;
}

/// Audio decoder trait
pub trait Decoder: Send {
    /// Decode compressed data to PCM samples
    fn decode(&mut self, data: &[u8], output: &mut [i16]) -> Result<usize>;

    /// Decode with packet loss concealment (no data available)
    fn decode_plc(&mut self, output: &mut [i16]) -> Result<usize>;

    /// Reset decoder state
    fn reset(&mut self) -> Result<()>;
}

/// Opus codec implementation
pub struct OpusCodec {
    config: AudioConfig,
}

impl OpusCodec {
    /// Create a new Opus codec
    pub fn new(config: AudioConfig) -> Result<Self> {
        Ok(Self { config })
    }

    /// Create an encoder
    pub fn create_encoder(&self) -> Result<OpusEncoder> {
        OpusEncoder::new(&self.config)
    }

    /// Create a decoder
    pub fn create_decoder(&self) -> Result<OpusDecoder> {
        OpusDecoder::new(&self.config)
    }
}

/// Convert OpusApplication to opus crate Application
fn opus_application(app: OpusApplication) -> opus::Application {
    match app {
        OpusApplication::Voip => opus::Application::Voip,
        OpusApplication::Audio => opus::Application::Audio,
        OpusApplication::LowDelay => opus::Application::LowDelay,
    }
}

/// Convert channel count to opus Channels
fn opus_channels(channels: u16) -> Result<opus::Channels> {
    match channels {
        1 => Ok(opus::Channels::Mono),
        2 => Ok(opus::Channels::Stereo),
        _ => Err(AudioError::InvalidConfig(format!(
            "Invalid channel count: {}. Must be 1 (mono) or 2 (stereo)",
            channels
        ))),
    }
}

/// Opus encoder
pub struct OpusEncoder {
    inner: opus::Encoder,
    channels: u16,
    frame_size: usize,
}

impl OpusEncoder {
    /// Create a new Opus encoder
    pub fn new(config: &AudioConfig) -> Result<Self> {
        let channels = opus_channels(config.channels)?;
        let application = opus_application(config.opus_application);

        let mut encoder = opus::Encoder::new(config.sample_rate, channels, application)
            .map_err(|e| AudioError::Codec(format!("Failed to create Opus encoder: {}", e)))?;

        // Set bitrate
        encoder
            .set_bitrate(opus::Bitrate::Bits(config.bitrate as i32))
            .map_err(|e| AudioError::Codec(format!("Failed to set bitrate: {}", e)))?;

        // Enable inband FEC for better packet loss handling
        encoder
            .set_inband_fec(true)
            .map_err(|e| AudioError::Codec(format!("Failed to enable FEC: {}", e)))?;

        // Set packet loss percentage expectation (helps encoder optimize)
        encoder
            .set_packet_loss_perc(5)
            .map_err(|e| AudioError::Codec(format!("Failed to set packet loss: {}", e)))?;

        Ok(Self {
            inner: encoder,
            channels: config.channels,
            frame_size: config.frame_size_samples(),
        })
    }

    /// Set the bitrate
    pub fn set_bitrate(&mut self, bitrate: u32) -> Result<()> {
        self.inner
            .set_bitrate(opus::Bitrate::Bits(bitrate as i32))
            .map_err(|e| AudioError::Codec(format!("Failed to set bitrate: {}", e)))?;
        Ok(())
    }

    /// Get expected packet size (estimate based on bitrate)
    pub fn packet_size(&self) -> usize {
        // Opus typically produces 20-200 bytes per 20ms frame at voice bitrates
        // This is just an estimate for buffer allocation
        256
    }
}

impl Encoder for OpusEncoder {
    fn encode(&mut self, pcm: &[i16], output: &mut [u8]) -> Result<usize> {
        let expected_samples = self.frame_size * self.channels as usize;
        if pcm.len() < expected_samples {
            return Err(AudioError::Encode(format!(
                "Not enough samples: got {}, expected {}",
                pcm.len(),
                expected_samples
            )));
        }

        let len = self
            .inner
            .encode(&pcm[..expected_samples], output)
            .map_err(|e| AudioError::Encode(format!("Opus encode error: {}", e)))?;

        Ok(len)
    }

    fn reset(&mut self) -> Result<()> {
        self.inner
            .reset_state()
            .map_err(|e| AudioError::Codec(format!("Failed to reset encoder: {}", e)))?;
        Ok(())
    }
}

/// Opus decoder
pub struct OpusDecoder {
    inner: opus::Decoder,
    channels: u16,
    frame_size: usize,
}

impl OpusDecoder {
    /// Create a new Opus decoder
    pub fn new(config: &AudioConfig) -> Result<Self> {
        let channels = opus_channels(config.channels)?;

        let decoder = opus::Decoder::new(config.sample_rate, channels)
            .map_err(|e| AudioError::Codec(format!("Failed to create Opus decoder: {}", e)))?;

        Ok(Self {
            inner: decoder,
            channels: config.channels,
            frame_size: config.frame_size_samples(),
        })
    }
}

impl Decoder for OpusDecoder {
    fn decode(&mut self, data: &[u8], output: &mut [i16]) -> Result<usize> {
        let samples = self
            .inner
            .decode(data, output, false)
            .map_err(|e| AudioError::Decode(format!("Opus decode error: {}", e)))?;

        // samples is per channel, return total samples written
        Ok(samples * self.channels as usize)
    }

    fn decode_plc(&mut self, output: &mut [i16]) -> Result<usize> {
        // Packet loss concealment: pass None as data
        // The frame_size parameter tells opus how many samples to generate
        let samples = self
            .inner
            .decode(&[], output, true) // fec=true enables forward error correction
            .map_err(|e| AudioError::Decode(format!("Opus PLC error: {}", e)))?;

        Ok(samples * self.channels as usize)
    }

    fn reset(&mut self) -> Result<()> {
        self.inner
            .reset_state()
            .map_err(|e| AudioError::Codec(format!("Failed to reset decoder: {}", e)))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encoder_creation() {
        let config = AudioConfig::default();
        let encoder = OpusEncoder::new(&config).unwrap();
        assert_eq!(encoder.sample_rate, 48000);
    }
}
