//! ffmpeg → VP8 frames, paced to real time.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use rtc::media::io::ivf_reader::IVFReader;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::{Error, Result};

/// What the encoder produces. Every viewer gets exactly this; there is no
/// per-viewer adaptation in P2P mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderConfig {
    /// Output height; the width follows the aspect ratio.
    pub height: u32,
    pub fps: u32,
    /// Target bitrate in kbit/s.
    pub bitrate_kbps: u32,
    /// Seconds between keyframes: the longest a joining viewer waits for a
    /// picture.
    pub keyframe_interval_secs: u32,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self { height: 720, fps: 30, bitrate_kbps: 1500, keyframe_interval_secs: 2 }
    }
}

/// Progress bar shown at the bottom of the video: how much of the track
/// has played and how much is left. `total_secs` is the track length,
/// `offset_secs` where playback starts (seek position).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoProgress {
    pub total_secs: f64,
    pub offset_secs: f64,
}

impl VideoProgress {
    pub fn new(total_secs: f64, offset_secs: f64) -> Option<Self> {
        if !total_secs.is_finite() || !offset_secs.is_finite() || total_secs <= 0.0 {
            return None;
        }
        Some(Self {
            total_secs,
            offset_secs: offset_secs.clamp(0.0, total_secs),
        })
    }
}

/// Where the video comes from.
#[derive(Debug)]
pub enum VideoInput {
    /// A generated test pattern.
    TestPattern,
    /// A generated source: an ffmpeg lavfi filter graph, such as a waiting
    /// screen. It is scaled to the configured height like any other input.
    Lavfi(String),
    /// Anything ffmpeg can open: a file, a direct media URL.
    Ffmpeg(String),
    /// A command whose stdout is a media container, piped into ffmpeg —
    /// typically `yt-dlp -o - <url>`.
    Command(Command),
    /// Like [`VideoInput::Command`], but with a progress bar burned into the
    /// bottom of the picture (elapsed + time left).
    CommandWithProgress {
        command: Command,
        progress: VideoProgress,
    },
}

impl VideoInput {
    /// Segments of the progress bar fill. `drawbox` has no per-frame `eval`:
    /// its geometry is frozen at init, so one box with an animated width can
    /// never work. Each segment is instead switched on by its own time window
    /// (`enable` is evaluated per frame with the real timestamp).
    pub const PROGRESS_SEGS: u32 = 40;

    /// Filter chain suffix that draws the progress bar along the bottom edge,
    /// or empty when there is nothing to show. Public so callers can
    /// smoke-test it with ffmpeg.
    pub fn progress_filter(progress: Option<VideoProgress>, height: u32) -> String {
        let Some(p) = progress else {
            return String::new();
        };
        let total = p.total_secs.max(1.0);
        let off = p.offset_secs.clamp(0.0, total);
        // Bar thickness scales with the picture, time text just above it.
        let bar_h = (height / 48).clamp(8, 18);
        let font = (height / 32).clamp(12, 24);
        let text_y = format!("h-{bar_h}-{font}-8");
        // Elapsed time for this encode run is `t` (starts at 0 after a seek),
        // so absolute position is `t+off`. Commas inside expressions must be
        // escaped (`\,`) because they would otherwise split filter options,
        // and colons inside drawtext must be `\:` for the same reason.
        // In drawbox `h`/`w` are the box's own size, not the frame's: the
        // frame is `ih`/`iw` (`y=h-N` would pin the bar to the top).
        let mut f = format!(",drawbox=x=0:y=ih-{bar_h}:w=iw:h={bar_h}:color=black@0.6:t=fill");
        for k in 0..Self::PROGRESS_SEGS {
            // Segment k lights up once (k+1)/SEGS of the track has played:
            // empty at the start, full exactly at the end.
            f.push_str(&format!(
                ",drawbox=x='iw*{k}/{}':y=ih-{bar_h}:w='iw/{}+1':h={bar_h}:color=0xe50914:t=fill:enable='gte(t+{off:.3},{total:.3}*{kp}/{})'",
                Self::PROGRESS_SEGS,
                Self::PROGRESS_SEGS,
                Self::PROGRESS_SEGS,
                kp = k + 1,
            ));
        }
        f.push_str(&format!(
            ",drawtext=font=Sans:text='- %{{eif\\:max(0\\,{total:.3}-(t+{off:.3}))/60\\:d}}\\:%{{eif\\:mod(max(0\\,{total:.3}-(t+{off:.3}))\\,60)\\:d\\:2}}':\
            fontcolor=white:fontsize={font}:x=(w-tw)/2:y={text_y}:\
            box=1:boxcolor=black@0.6:boxborderw=4"
        ));
        f
    }
}

/// A running encode. Dropping it kills ffmpeg and the input command.
pub struct VideoEncoder {
    /// Always `Some` until dropped; the option lets `drop` move it out.
    ffmpeg: Option<Child>,
    input: Option<Child>,
    state: Arc<Shared>,
}

#[derive(Default)]
struct Shared {
    cancelled: AtomicBool,
    paused: AtomicBool,
    started: AtomicBool,
    finished: AtomicBool,
}

impl VideoEncoder {
    pub(crate) fn spawn(
        input: VideoInput,
        config: EncoderConfig,
        frames: broadcast::Sender<Bytes>,
    ) -> Result<Self> {
        let mut ffmpeg = Command::new("ffmpeg");
        ffmpeg.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
        let mut input_child = None;
        let mut progress: Option<VideoProgress> = None;
        match input {
            VideoInput::TestPattern => {
                ffmpeg.args(["-f", "lavfi", "-i", &format!("testsrc2=size=1280x720:rate={}", config.fps)]);
                ffmpeg.stdin(Stdio::null());
            }
            VideoInput::Lavfi(graph) => {
                ffmpeg.args(["-f", "lavfi", "-i", &graph]);
                ffmpeg.stdin(Stdio::null());
            }
            VideoInput::Ffmpeg(input) => {
                ffmpeg.args(["-i", &input]);
                ffmpeg.stdin(Stdio::null());
            }
            VideoInput::Command(command) => {
                spawn_pipe_input(command, &mut ffmpeg, &mut input_child)?;
            }
            VideoInput::CommandWithProgress { command, progress: p } => {
                progress = Some(p);
                spawn_pipe_input(command, &mut ffmpeg, &mut input_child)?;
            }
        }

        let bitrate = format!("{}k", config.bitrate_kbps);
        let gop = (config.fps * config.keyframe_interval_secs).to_string();
        let base_vf = format!("scale=-2:{},fps={}", config.height, config.fps);
        let full_vf = format!("{base_vf}{}", VideoInput::progress_filter(progress, config.height));
        ffmpeg.args(["-an", "-vf", &full_vf]);
        ffmpeg.args(["-c:v", "libvpx", "-deadline", "realtime", "-cpu-used", "8"]);
        ffmpeg.args(["-b:v", &bitrate, "-maxrate", &bitrate, "-bufsize", &bitrate]);
        ffmpeg.args(["-g", &gop, "-keyint_min", &gop]);
        // No lookahead and no alt-ref frames: every frame is sendable as soon
        // as it is encoded, and a lost packet does not poison later frames.
        ffmpeg.args(["-error-resilient", "1", "-auto-alt-ref", "0", "-lag-in-frames", "0"]);
        // No `-re`: the reader below paces by timestamp, which is what makes
        // pause exact. ffmpeg runs ahead only as far as the pipe lets it.
        ffmpeg.args(["-flush_packets", "1", "-f", "ivf", "pipe:1"]);
        ffmpeg.stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut ffmpeg = match ffmpeg.spawn() {
            Ok(child) => child,
            Err(source) => {
                if let Some(mut child) = input_child {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                return Err(Error::Spawn { program: "ffmpeg".into(), source });
            }
        };
        let stdout = ffmpeg.stdout.take().expect("stdout is piped");
        let stderr = ffmpeg.stderr.take().expect("stderr is piped");

        log_lines("ffmpeg".into(), stderr);

        let state = Arc::new(Shared::default());
        let reader_state = state.clone();
        std::thread::spawn(move || read_frames(stdout, &frames, &reader_state));

        Ok(Self { ffmpeg: Some(ffmpeg), input: input_child, state })
    }

    /// Whether the first frame has gone out. Useful to start audio that
    /// travels another way (the voice channel) in step with the picture.
    pub fn has_started(&self) -> bool {
        self.state.started.load(Ordering::Acquire)
    }

    /// Whether the input ran out (or failed).
    pub fn has_finished(&self) -> bool {
        self.state.finished.load(Ordering::Acquire)
    }

    pub(crate) fn set_paused(&self, paused: bool) {
        self.state.paused.store(paused, Ordering::Release);
    }
}

impl Drop for VideoEncoder {
    fn drop(&mut self) {
        self.state.cancelled.store(true, Ordering::Release);
        let children: Vec<Child> = self.ffmpeg.take().into_iter().chain(self.input.take()).collect();
        // Reap off-thread: `wait` after `kill` is quick, but not free.
        std::thread::spawn(move || {
            for mut child in children {
                let _ = child.kill();
                let _ = child.wait();
            }
        });
    }
}

/// Starts `command` (typically yt-dlp) and wires its stdout to ffmpeg's
/// stdin as `pipe:0`. Shared by `Command` and `CommandWithProgress`.
fn spawn_pipe_input(
    mut command: Command,
    ffmpeg: &mut Command,
    input_child: &mut Option<Child>,
) -> Result<()> {
    let program = command.get_program().to_string_lossy().into_owned();
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| Error::Spawn { program: program.clone(), source })?;
    let stdout = child.stdout.take().expect("stdout is piped");
    // An input that ends early (a dropped download, say) looks like
    // a normal end of video to ffmpeg; its own stderr says why.
    log_lines(program, child.stderr.take().expect("stderr is piped"));
    ffmpeg.args(["-i", "pipe:0"]);
    ffmpeg.stdin(Stdio::from(stdout));
    *input_child = Some(child);
    Ok(())
}

/// Forwards a child's stderr to the log, line by line, until it closes.
fn log_lines(program: String, stderr: impl std::io::Read + Send + 'static) {
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(|l| l.ok()) {
            warn!("{program}: {line}");
        }
    });
}

/// Publishes each frame at its presentation time. A pause freezes the clock
/// rather than letting it run, so there is no catch-up burst on resume.
fn read_frames(stdout: std::process::ChildStdout, frames: &broadcast::Sender<Bytes>, state: &Shared) {
    let (mut ivf, header) = match IVFReader::new(BufReader::new(stdout)) {
        Ok(reader) => reader,
        Err(error) => {
            if !state.cancelled.load(Ordering::Acquire) {
                warn!(%error, "ffmpeg produced no video");
            }
            state.finished.store(true, Ordering::Release);
            return;
        }
    };
    debug!(width = header.width, height = header.height, "Encoder running");
    let tick = |pts: u64| {
        Duration::from_secs_f64(
            pts as f64 * f64::from(header.timebase_numerator) / f64::from(header.timebase_denominator),
        )
    };

    let mut clock: Option<(Instant, u64)> = None;
    while let Ok((frame, frame_header)) = ivf.parse_next_frame() {
        let (start, first_pts) = *clock.get_or_insert((Instant::now(), frame_header.timestamp));
        let mut due = start + tick(frame_header.timestamp.saturating_sub(first_pts));
        loop {
            if state.cancelled.load(Ordering::Acquire) {
                return;
            }
            if state.paused.load(Ordering::Acquire) {
                let paused_at = Instant::now();
                while state.paused.load(Ordering::Acquire) && !state.cancelled.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(10));
                }
                let pause = paused_at.elapsed();
                if let Some((start, _)) = clock.as_mut() {
                    *start += pause;
                }
                due += pause;
                continue;
            }
            let now = Instant::now();
            if due <= now {
                break;
            }
            // Short sleeps so a pause or a stop is noticed promptly.
            std::thread::sleep((due - now).min(Duration::from_millis(10)));
        }
        if !state.started.swap(true, Ordering::AcqRel) {
            info!("First video frame out");
        }
        // Sending with no viewers subscribed is not an error worth reporting.
        let _ = frames.send(frame.freeze());
    }
    state.finished.store(true, Ordering::Release);
}
