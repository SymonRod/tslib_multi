//! Screen sharing protocol capture tool.
//!
//! Research tool for Phase 2 of the TS6 screen sharing work (the plan lives in
//! `TS6_Droid/docs/screenshare-phase2-plan.md`). It connects to a server and
//! writes every command packet — incoming and outgoing, decrypted,
//! defragmented and decompressed — to a transcript, *before* `tsclientlib`
//! tries to parse it.
//!
//! That last point is the whole reason this exists: `InMessage` is generated
//! from a closed list of known commands, so `notifystreamstarted` and friends
//! fail to parse and are dropped, arguments and all, before any `StreamItem`
//! is produced. The hook used here is `tsproto`'s event listener, which fires
//! on the raw `InPacket` — no patched fork of `tsclientlib` needed.
//!
//! Commands typed on stdin are sent verbatim as command packets, so the
//! client-to-server half of the exchange (`joinstreamrequest`,
//! `streamsignaling`, …) can be probed without recompiling.

use std::fs::File;
use std::io::{BufRead, BufWriter, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::Parser;
use futures::StreamExt;
use tracing::{info, warn};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

use tsclientlib::prelude::*;
use tsclientlib::{Connection as TsConnection, DisconnectOptions, Reason, StreamItem};
use tslib_core::Identity;
use tsproto::connection::Event as ProtoEvent;
use tsproto_packets::packets::{Direction, Flags, OutCommand, PacketType};

/// Command name prefixes that are interesting for this capture. Anything
/// matching is highlighted on stdout; everything is written to the transcript
/// regardless.
const HIGHLIGHT: &[&str] = &["stream", "sfu", "video", "screen"];

/// Capture the screen sharing signaling of a TeamSpeak session
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
	/// Server address (host or host:port)
	#[arg(short, long)]
	server: String,

	/// Nickname to connect with
	#[arg(short, long, default_value = "StreamCapture")]
	nickname: String,

	/// Server password
	#[arg(short, long)]
	password: Option<String>,

	/// Channel to join (the sharer's channel, usually)
	#[arg(short, long)]
	channel: Option<String>,

	/// Identity file (created if missing)
	#[arg(short, long, default_value = "identity.json")]
	identity: String,

	/// Transcript basename; `.jsonl` and `.log` are appended
	#[arg(short, long, default_value = "stream-capture")]
	out: String,

	/// Also print every command to stdout, not just the interesting ones
	#[arg(short, long)]
	verbose: bool,
}

/// Writes one JSON object per command packet.
struct Transcript {
	file: Mutex<BufWriter<File>>,
	start: Instant,
}

impl Transcript {
	fn create(path: &str) -> Result<Self> {
		let file = File::create(path).with_context(|| format!("cannot create {path}"))?;
		Ok(Self { file: Mutex::new(BufWriter::new(file)), start: Instant::now() })
	}

	fn record(&self, dir: &str, p_type: PacketType, content: &[u8]) {
		let name = command_name(content);
		let entry = serde_json::json!({
			"t_rel": self.start.elapsed().as_secs_f64(),
			"epoch_ms": SystemTime::now()
				.duration_since(UNIX_EPOCH)
				.map(|d| d.as_millis() as u64)
				.unwrap_or(0),
			"dir": dir,
			"type": format!("{p_type:?}"),
			"name": name,
			// The command as it went over the wire, after decryption and
			// decompression: TS3 escaping still intact, nothing reordered.
			"raw": String::from_utf8_lossy(content),
			// Kept for anything that is not valid UTF-8 — a JSON body with
			// binary in it would show up here and nowhere else.
			"hex": hex(content),
		});

		if let Ok(mut file) = self.file.lock() {
			let _ = writeln!(file, "{entry}");
			// Flushed per line: a capture that dies mid-session is still
			// evidence, and the volume is far too low for this to matter.
			let _ = file.flush();
		}
	}
}

fn hex(data: &[u8]) -> String {
	let mut s = String::with_capacity(data.len() * 2);
	for b in data {
		s.push_str(&format!("{b:02x}"));
	}
	s
}

/// The command name is everything up to the first space or pipe.
fn command_name(content: &[u8]) -> String {
	let end = content.iter().position(|c| *c == b' ' || *c == b'|').unwrap_or(content.len());
	String::from_utf8_lossy(&content[..end]).to_string()
}

fn is_interesting(name: &str) -> bool {
	let lower = name.to_lowercase();
	HIGHLIGHT.iter().any(|k| lower.contains(k))
}

#[tokio::main]
async fn main() -> Result<()> {
	let args = Args::parse();

	let jsonl_path = format!("{}.jsonl", args.out);
	let log_path = format!("{}.log", args.out);

	// Two sinks: the terminal, filtered by RUST_LOG, and a debug-level log
	// file. The log file matters because `log_commands` below starts logging
	// from the very first packet, which includes the `initserver` we cannot
	// reach with an event listener — the connection does not exist yet when
	// the handshake runs, so `virtualserver_capability_extensions` is only
	// visible there.
	let log_file = File::create(&log_path).with_context(|| format!("cannot create {log_path}"))?;
	tracing_subscriber::registry()
		.with(
			tracing_subscriber::fmt::layer()
				.with_target(false)
				.with_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())),
		)
		.with(
			tracing_subscriber::fmt::layer()
				.with_ansi(false)
				.with_writer(log_file)
				.with_filter(EnvFilter::new("debug")),
		)
		.init();

	let identity = if std::path::Path::new(&args.identity).exists() {
		info!(path = %args.identity, "Loading identity");
		Identity::load(&args.identity)?
	} else {
		info!("Creating a new identity");
		let identity = Identity::create()?;
		identity.save(&args.identity)?;
		identity
	};
	info!(uid = %identity.unique_id(), "Identity ready");

	let transcript = Arc::new(Transcript::create(&jsonl_path)?);

	let mut options = TsConnection::build(args.server.clone())
		.name(args.nickname.clone())
		.identity(identity.to_ts_identity())
		// Sends every command through `tsproto::log` at debug level, which the
		// log file above picks up. Redundant with the transcript for anything
		// after the handshake; the only source for the handshake itself.
		.log_commands(true);
	if let Some(password) = args.password.clone() {
		options = options.password(password);
	}
	if let Some(channel) = args.channel.clone() {
		options = options.channel(channel);
	}

	info!(server = %args.server, "Connecting");
	let mut con = options.connect()?;

	// The hook. `Event::ReceivePacket` fires once per reassembled command,
	// after decryption and decompression and before `InMessage::new` — so
	// unknown commands arrive here with their arguments intact.
	{
		let client = con.get_tsproto_client_mut()?;
		let transcript = transcript.clone();
		let verbose = args.verbose;
		let listener: Box<dyn for<'a> Fn(&'a ProtoEvent<'a>) + Send> =
			Box::new(move |event: &ProtoEvent| {
				let (dir, p_type, content) = match event {
					ProtoEvent::ReceivePacket(p) => ("in", p.header().packet_type(), p.content()),
					ProtoEvent::SendPacket(p) => ("out", p.header().packet_type(), p.content()),
					_ => return,
				};
				if !p_type.is_command() {
					return;
				}
				transcript.record(dir, p_type, content);

				let name = command_name(content);
				if is_interesting(&name) {
					println!("\n>>> {dir} {name}\n{}\n", String::from_utf8_lossy(content));
				} else if verbose {
					println!("{dir} {}", String::from_utf8_lossy(content));
				}
			});
		client.event_listeners.push(listener);
	}

	info!(transcript = %jsonl_path, log = %log_path, "Connected — capturing");
	println!(
		"Type a raw command to send it verbatim (e.g. `joinstreamrequest streamid=1`).\n\
		 `/clients` lists clients, `/quit` disconnects."
	);

	// stdin is read on a blocking thread; the connection is not `Send`, so
	// everything else stays on this task.
	let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
	std::thread::spawn(move || {
		let stdin = std::io::stdin();
		for line in stdin.lock().lines() {
			match line {
				Ok(line) => {
					if tx.send(line).is_err() {
						break;
					}
				}
				Err(_) => break,
			}
		}
	});

	let stop = Arc::new(AtomicBool::new(false));
	{
		let stop = stop.clone();
		tokio::spawn(async move {
			if tokio::signal::ctrl_c().await.is_ok() {
				stop.store(true, Ordering::SeqCst);
			}
		});
	}

	while !stop.load(Ordering::SeqCst) {
		// Drain the event stream. Everything worth capturing already went
		// through the listener; this loop only keeps the book up to date and
		// notices a dropped connection.
		match tokio::time::timeout(Duration::from_millis(100), con.events().next()).await {
			Ok(Some(Ok(StreamItem::DisconnectedTemporarily(reason)))) => {
				warn!(?reason, "Disconnected temporarily, reconnecting");
			}
			Ok(Some(Ok(_))) => {}
			Ok(Some(Err(error))) => warn!(%error, "Event stream error"),
			Ok(None) => {
				warn!("Connection closed by the server");
				break;
			}
			Err(_) => {}
		}

		while let Ok(line) = rx.try_recv() {
			let line = line.trim().to_string();
			if line.is_empty() {
				continue;
			}
			match line.as_str() {
				"/quit" => {
					stop.store(true, Ordering::SeqCst);
				}
				"/clients" => print_clients(&con),
				_ => {
					// `OutCommand::new` appends the name bytes verbatim, so
					// passing the whole line through it sends exactly what was
					// typed — TS3 escapes (`\s`, `\p`) included.
					let cmd =
						OutCommand::new(Direction::C2S, Flags::empty(), PacketType::Command, &line);
					match cmd.send(&mut con) {
						Ok(()) => info!(command = %line, "Sent"),
						Err(error) => warn!(%error, "Failed to send command"),
					}
				}
			}
		}
	}

	info!("Disconnecting");
	let _ = con.disconnect(
		DisconnectOptions::new().reason(Reason::Clientdisconnect).message("capture done"),
	);
	// Give the disconnect packet a moment to leave.
	let _ = tokio::time::timeout(Duration::from_secs(2), con.events().next()).await;

	println!("Transcript: {jsonl_path}\nLog: {log_path}");
	Ok(())
}

fn print_clients(con: &TsConnection) {
	let state = match con.get_state() {
		Ok(s) => s,
		Err(error) => {
			warn!(%error, "No state yet");
			return;
		}
	};
	println!("--- clients ---");
	for (id, client) in &state.clients {
		let channel = client.channel.0;
		println!("  id={:<5} channel={:<5} {}", id.0, channel, client.name);
	}
	println!("--- channels ---");
	for (id, channel) in &state.channels {
		println!("  id={:<5} {}", id.0, channel.name);
	}
}
