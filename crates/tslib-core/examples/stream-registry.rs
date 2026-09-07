use anyhow::{bail, Context, Result};
use std::collections::HashSet;
use std::time::Duration;
use tokio::time::{timeout, Instant};
use tsclientlib::{messages::c2s, prelude::*, ClientId};
use tslib_core::{events::Event, Client, ClientConfig, Identity};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    // Avoid protocol debug logs, which could expose SDP/ICE.
    tracing_subscriber::fmt()
        .with_env_filter("tslib_core=warn")
        .init();

    let mut args = std::env::args().skip(1);
    let server = args
        .next()
        .context("Usage: stream-registry <server> [channel] [duration-seconds=90]")?;
    let channel = args.next().filter(|channel| !channel.is_empty());
    let duration = args
        .next()
        .map(|seconds| seconds.parse::<u64>())
        .transpose()
        .context("Duration must be an unsigned integer in seconds")?
        .unwrap_or(90);
    if args.next().is_some() {
        bail!("Usage: stream-registry <server> [channel] [duration-seconds=90]");
    }

    let mut config = ClientConfig::builder()
        .address(server)
        .identity(Identity::create().context("Creating ephemeral identity")?)
        .nickname("TS6DroidRegistry");
    if let Some(channel) = channel {
        config = config.channel(channel);
    }
    let mut client = Client::connect(config.build()?).context("Starting connection")?;
    let mut observed = false;
    let result: Result<()> = async {
        timeout(Duration::from_secs(30), client.wait_connected())
            .await
            .context("Connection timed out after 30 seconds")?
            .context("Connection failed")?;

        let streams = client.streams();
        println!("Initial snapshot: {streams:#?}");
        observed |= !streams.is_empty();
        let mut requested = HashSet::new();
        let mut next_request = Instant::now();
        let listen = async {
            loop {
                // users() reads Client's cache, including subsequent book updates.
                if Instant::now() >= next_request {
                    if let Some(owner) = client
                        .users()
                        .iter()
                        .filter(|user| user.is_streaming && !requested.contains(&user.id))
                        .map(|user| user.id)
                        .min()
                    {
                        requested.insert(owner);
                        c2s::OutRequestStreamInfoPart {
                            client_id: ClientId(owner),
                            stream_id: None,
                        }
                        .send(client.inner_mut().context("Connection unavailable")?)
                        .context("Sending requeststreaminfo")?;
                        println!("Requested stream info for owner {owner}");
                        next_request = Instant::now() + Duration::from_secs(4);
                    }
                }

                for event in client.process_events().await.context("Processing events")? {
                    match event {
                        Event::StreamsChanged { streams } => {
                            observed |= !streams.is_empty();
                            println!("StreamsChanged: {streams:#?}");
                        }
                        Event::Disconnected { reason } | Event::ConnectionLost { reason } => {
                            bail!("Connection ended: {reason}");
                        }
                        Event::CommandError { error_id, message } => {
                            bail!("Server command error {error_id}: {message}");
                        }
                        _ => {}
                    }
                }
            }
        };
        match timeout(Duration::from_secs(duration), listen).await {
            Ok(result) => result,
            Err(_) => Ok(()),
        }
    }
    .await;

    // Snapshot before disconnect, which intentionally clears the registry.
    let streams = client.streams();
    observed |= !streams.is_empty();
    println!("Final snapshot (before disconnect): {streams:#?}");
    let disconnect = client.disconnect().context("Disconnect failed");
    if disconnect.is_ok() {
        let _ = timeout(Duration::from_secs(2), async {
            loop {
                match client.process_events().await {
                    Ok(events) => {
                        if events
                            .iter()
                            .any(|event| matches!(event, Event::Disconnected { .. }))
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        eprintln!("Disconnect flush error: {error}");
                        break;
                    }
                }
            }
        })
        .await;
    }
    if !observed {
        eprintln!("Zero streams observed; registry discovery was not verified.");
    }
    result?;
    disconnect?;
    if !observed {
        bail!("Zero streams observed");
    }
    println!("Observed stream metadata; no stream was joined and no media was started.");
    Ok(())
}
