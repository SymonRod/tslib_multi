//! Publishes a video as a TeamSpeak 6 screen share.
//!
//! The smallest complete broadcaster: `tslib-core` for the signaling,
//! `tslib-stream` for the media. Point a TS6 client at the channel and open
//! the stream.
//!
//! ```text
//! cargo run -p stream-broadcast -- --server host:port --channel Lobby
//! cargo run -p stream-broadcast -- --server host:port --youtube <url>
//! ```

use std::process::Command;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use tracing::{info, warn};

use tslib_core::{Client, ClientConfig, Event, Identity, StreamSetup};
use tslib_stream::{BroadcastConfig, BroadcastEvent, Broadcaster, EncoderConfig, VideoInput};

/// Publish a video as a TeamSpeak 6 screen share
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
	/// Server address (host or host:port)
	#[arg(short, long)]
	server: String,

	/// Nickname to connect with
	#[arg(short, long, default_value = "StreamBot")]
	nickname: String,

	/// Server password
	#[arg(short, long)]
	password: Option<String>,

	/// Channel to join
	#[arg(short, long)]
	channel: Option<String>,

	/// Identity file (created if missing)
	#[arg(short, long, default_value = "identity.json")]
	identity: String,

	/// Stream name shown to viewers
	#[arg(long, default_value = "Test")]
	name: String,

	/// Anything ffmpeg can open. Defaults to a generated test pattern.
	#[arg(long, conflicts_with = "youtube")]
	input: Option<String>,

	/// A YouTube (or any yt-dlp) page URL
	#[arg(long)]
	youtube: Option<String>,

	/// Output height; width follows the aspect ratio
	#[arg(long, default_value_t = 720)]
	height: u32,

	/// Target video bitrate in kbit/s, per viewer
	#[arg(long, default_value_t = 1500)]
	bitrate: u32,

	/// Local address for WebRTC, e.g. 192.168.1.10:0. Default: every interface.
	#[arg(long)]
	bind: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
	tracing_subscriber::fmt()
		.with_target(false)
		.with_env_filter(
			tracing_subscriber::EnvFilter::try_from_default_env()
				.unwrap_or_else(|_| "info,webrtc=warn,rtc=warn".into()),
		)
		.init();
	let args = Args::parse();

	let identity = if std::path::Path::new(&args.identity).exists() {
		Identity::load(&args.identity)?
	} else {
		let identity = Identity::create()?;
		identity.save(&args.identity)?;
		identity
	};
	let mut config = ClientConfig::builder()
		.address(&args.server)
		.identity(identity)
		.nickname(&args.nickname);
	if let Some(password) = args.password.clone() {
		config = config.password(password);
	}
	if let Some(channel) = args.channel.clone() {
		config = config.channel(channel);
	}
	let mut client = Client::connect(config.build()?)?;
	client.wait_connected().await?;
	info!("Connected");

	let mut broadcast_config = BroadcastConfig {
		encoder: EncoderConfig { height: args.height, bitrate_kbps: args.bitrate, ..Default::default() },
		..Default::default()
	};
	if let Some(bind) = args.bind.clone() {
		broadcast_config.bind_addrs = vec![bind];
	}
	let mut broadcaster = Broadcaster::new(broadcast_config);
	broadcaster.play(match (&args.input, &args.youtube) {
		(_, Some(url)) => {
			let mut ytdlp = Command::new("yt-dlp");
			ytdlp.args(["-f", &format!("bv*[height<={}]/b", args.height), "-o", "-", "--no-playlist", "-q", url]);
			VideoInput::Command(ytdlp)
		}
		(Some(input), None) => VideoInput::Ffmpeg(input.clone()),
		(None, None) => VideoInput::TestPattern,
	})?;

	client.setup_stream(&StreamSetup::new(args.name.clone(), args.bitrate * 1000))?;
	let mut stream_id: Option<String> = None;

	let mut tick = tokio::time::interval(Duration::from_millis(20));
	loop {
		tokio::select! {
			_ = tokio::signal::ctrl_c() => break,
			_ = tick.tick() => {}
		}

		for event in client.process_events().await? {
			match event {
				Event::StreamsChanged { .. } => {
					let own = client.own_streams().into_iter().next().map(|s| s.id);
					if own != stream_id {
						info!(id = ?own, "Own stream");
						stream_id = own;
					}
				}
				Event::StreamJoinRequest { viewer_id, stream_id: id, is_remove } => {
					if Some(&id) != stream_id.as_ref() {
						continue;
					}
					if is_remove {
						info!(viewer_id, "Viewer left");
						broadcaster.remove_viewer(viewer_id);
					} else {
						info!(viewer_id, "Join request");
						broadcaster.add_viewer(viewer_id);
					}
				}
				Event::StreamSignaling { owner_id: viewer_id, stream_id: id, json } => {
					if Some(&id) == stream_id.as_ref() {
						broadcaster.handle_signaling(viewer_id, &json);
					}
				}
				Event::UserLeft { user, .. } => {
					broadcaster.remove_viewer(user.id);
				}
				Event::Disconnected { reason } | Event::ConnectionLost { reason } => {
					warn!(%reason, "Disconnected");
					return Ok(());
				}
				_ => {}
			}
		}

		while let Some(event) = broadcaster.poll_event() {
			match event {
				BroadcastEvent::Offer { viewer_id, sdp } => {
					if let Some(id) = &stream_id {
						client.accept_stream_viewer(viewer_id, id, &sdp)?;
					}
				}
				BroadcastEvent::Connected { viewer_id } => info!(viewer_id, "Streaming to viewer"),
				BroadcastEvent::Gone { viewer_id, reason } => info!(viewer_id, %reason, "Viewer gone"),
			}
		}

		if broadcaster.video_finished() {
			info!("Video finished");
			break;
		}
	}

	if let Some(id) = &stream_id {
		client.stop_stream(id)?;
	}
	client.disconnect()?;
	for _ in 0..10 {
		let _ = client.process_events().await;
		tokio::time::sleep(Duration::from_millis(100)).await;
	}
	Ok(())
}
