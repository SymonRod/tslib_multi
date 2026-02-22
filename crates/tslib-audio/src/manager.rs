//! High-level audio manager

use crate::capture::{AudioFrame, CaptureDevice, VoiceActivityDetector};
use crate::codec::{Decoder, Encoder, OpusCodec, OpusDecoder, OpusEncoder};
use crate::config::AudioConfig;
use crate::error::{AudioError, Result};
use crate::mixer::AudioMixer;
use crate::playback::PlaybackDevice;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};

/// High-level audio manager
pub struct AudioManager {
    config: AudioConfig,
    /// Capture device
    capture: Arc<Mutex<CaptureDevice>>,
    /// Playback device
    playback: Arc<Mutex<PlaybackDevice>>,
    /// Encoder for outgoing audio
    encoder: Arc<Mutex<OpusEncoder>>,
    /// Decoders for incoming audio (per user)
    decoders: Arc<RwLock<HashMap<u16, OpusDecoder>>>,
    /// Audio mixer
    mixer: Arc<Mutex<AudioMixer>>,
    /// VAD
    vad: Arc<Mutex<VoiceActivityDetector>>,
    /// Is capturing
    is_capturing: Arc<RwLock<bool>>,
    /// Encoded packet sender
    packet_tx: Option<mpsc::Sender<EncodedPacket>>,
}

/// An encoded audio packet ready for transmission
#[derive(Debug, Clone)]
pub struct EncodedPacket {
    /// Encoded data
    pub data: Vec<u8>,
    /// Codec used
    pub codec: u8,
    /// Sequence number
    pub sequence: u16,
    /// Voice activity detected
    pub voice_activity: bool,
}

impl AudioManager {
    /// Create a new audio manager
    pub fn new(config: AudioConfig) -> Result<Self> {
        let capture = CaptureDevice::new(config.clone())?;
        let playback = PlaybackDevice::new(config.clone())?;
        let codec = OpusCodec::new(config.clone())?;
        let encoder = codec.create_encoder()?;
        let vad = VoiceActivityDetector::new(config.vad_threshold);
        let mixer = AudioMixer::new(config.sample_rate, config.playback_buffer_samples() * 4);

        Ok(Self {
            config,
            capture: Arc::new(Mutex::new(capture)),
            playback: Arc::new(Mutex::new(playback)),
            encoder: Arc::new(Mutex::new(encoder)),
            decoders: Arc::new(RwLock::new(HashMap::new())),
            mixer: Arc::new(Mutex::new(mixer)),
            vad: Arc::new(Mutex::new(vad)),
            is_capturing: Arc::new(RwLock::new(false)),
            packet_tx: None,
        })
    }

    /// Start audio capture
    pub async fn start_capture(&self) -> Result<mpsc::Receiver<EncodedPacket>> {
        let (packet_tx, packet_rx) = mpsc::channel(32);
        let (frame_tx, mut frame_rx) = mpsc::channel(32);

        // Start capture device
        self.capture.lock().await.start(frame_tx)?;
        *self.is_capturing.write().await = true;

        // Spawn encoding task
        let encoder = self.encoder.clone();
        let vad = self.vad.clone();
        let is_capturing = self.is_capturing.clone();
        let vad_enabled = self.config.vad_enabled;

        tokio::spawn(async move {
            let mut sequence = 0u16;
            let mut encode_buffer = vec![0u8; 1024];

            while *is_capturing.read().await {
                match frame_rx.recv().await {
                    Some(frame) => {
                        // Check VAD
                        let voice_activity = if vad_enabled {
                            vad.lock().await.process(&frame.samples)
                        } else {
                            true
                        };

                        // Only encode if voice detected (or VAD disabled)
                        if voice_activity {
                            let mut enc = encoder.lock().await;
                            match enc.encode(&frame.samples, &mut encode_buffer) {
                                Ok(len) => {
                                    let packet = EncodedPacket {
                                        data: encode_buffer[..len].to_vec(),
                                        codec: 4, // Opus Voice
                                        sequence,
                                        voice_activity,
                                    };
                                    sequence = sequence.wrapping_add(1);
                                    let _ = packet_tx.send(packet).await;
                                }
                                Err(e) => {
                                    tracing::warn!("Encoding error: {}", e);
                                }
                            }
                        }
                    }
                    None => break,
                }
            }
        });

        Ok(packet_rx)
    }

    /// Stop audio capture
    pub async fn stop_capture(&self) -> Result<()> {
        *self.is_capturing.write().await = false;
        self.capture.lock().await.stop()?;
        Ok(())
    }

    /// Start audio playback
    pub async fn start_playback(&self) -> Result<()> {
        self.playback.lock().await.start()
    }

    /// Stop audio playback
    pub async fn stop_playback(&self) -> Result<()> {
        self.playback.lock().await.stop()
    }

    /// Process incoming audio from a user
    pub async fn process_incoming(
        &self,
        user_id: u16,
        data: &[u8],
        codec: u8,
    ) -> Result<()> {
        // Get or create decoder for this user
        let mut decoders = self.decoders.write().await;
        let decoder = decoders
            .entry(user_id)
            .or_insert_with(|| OpusDecoder::new(&self.config).unwrap());

        // Decode
        let mut samples = vec![0i16; self.config.frame_size_samples()];
        let decoded = decoder.decode(data, &mut samples)?;

        // Add to mixer
        self.mixer.lock().await.add_audio(user_id, &samples[..decoded]);

        Ok(())
    }

    /// Handle packet loss for a user
    pub async fn handle_packet_loss(&self, user_id: u16) -> Result<()> {
        let mut decoders = self.decoders.write().await;
        if let Some(decoder) = decoders.get_mut(&user_id) {
            let mut samples = vec![0i16; self.config.frame_size_samples()];
            let decoded = decoder.decode_plc(&mut samples)?;
            self.mixer.lock().await.add_audio(user_id, &samples[..decoded]);
        }
        Ok(())
    }

    /// Set input volume
    pub async fn set_input_volume(&self, volume: f32) -> Result<()> {
        self.capture.lock().await.set_volume(volume)
    }

    /// Set output volume
    pub async fn set_output_volume(&self, volume: f32) -> Result<()> {
        self.playback.lock().await.set_volume(volume)
    }

    /// Set volume for a specific user
    pub async fn set_user_volume(&self, user_id: u16, volume: f32) {
        self.mixer.lock().await.set_user_volume(user_id, volume);
    }

    /// Mute/unmute a specific user
    pub async fn set_user_muted(&self, user_id: u16, muted: bool) {
        self.mixer.lock().await.set_user_muted(user_id, muted);
    }

    /// Remove a user's audio state
    pub async fn remove_user(&self, user_id: u16) {
        self.decoders.write().await.remove(&user_id);
        self.mixer.lock().await.remove_user(user_id);
    }

    /// Get VAD state
    pub async fn is_voice_active(&self) -> bool {
        self.vad.lock().await.energy() > self.config.vad_threshold
    }

    /// Get current voice energy level
    pub async fn voice_energy(&self) -> f32 {
        self.vad.lock().await.energy()
    }
}
