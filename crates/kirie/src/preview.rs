//! `kirie preview` — render a wallpaper into a socket instead of onto a screen.
//!
//! `--screenshot` already renders off-screen, but a process per frame is a
//! process per edit: a picker dragging a slider pays a full engine startup for
//! every value it passes through. This keeps one engine up, renders
//! continuously, and streams the frames to whoever connected — so a wallpaper
//! *animates* in the client, and a property change costs a rebuild rather than
//! a launch.
//!
//! It touches nothing on the desktop. There is no surface, no layer shell and
//! no control socket of the running engine involved; a preview and the
//! wallpaper actually up never meet.
//!
//! # The protocol
//!
//! One Unix stream socket, both directions at once.
//!
//! **Server → client**, per frame: a 24-byte little-endian header followed by
//! the pixels.
//!
//! ```text
//! magic   u32   b"KPV1"
//! seq     u32   frame number, from 1
//! width   u32
//! height  u32
//! format  u32   0 = RGBA8 (unorm, sRGB)
//! bytes   u32   payload length = width * height * 4
//! ```
//!
//! A header rather than a bare stream because the size changes: a client that
//! assumed the first frame's dimensions would tear the moment one arrived at
//! another size.
//!
//! **Client → server**, newline-terminated text, the same grammar the control
//! socket uses so nothing new has to be learned:
//!
//! ```text
//! property <key> <value>   set one, rebuild, keep rendering
//! bg <path>                preview something else
//! fps <n>                  1..=120
//! pause | resume
//! quit
//! ```
//!
//! A value is the rest of the line, so a colour travels as `0.5 0.25 1`
//! unquoted — the same rule as §4 of the control socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

/// What a frame header says about the frame behind it.
///
/// Public so the encoding has one definition rather than one per reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// Which frame this is, from 1.
    pub seq: u32,
    /// Pixels across.
    pub width: u32,
    /// Pixels down.
    pub height: u32,
    /// How many bytes of payload follow.
    pub bytes: u32,
}

/// The magic every frame starts with.
pub const MAGIC: [u8; 4] = *b"KPV1";

/// The only pixel format this speaks.
const FORMAT_RGBA8: u32 = 0;

/// How many bytes a header occupies.
pub const HEADER_BYTES: usize = 24;

impl FrameHeader {
    /// The header, as it goes on the wire.
    #[must_use]
    pub fn encode(self) -> [u8; HEADER_BYTES] {
        let mut out = [0_u8; HEADER_BYTES];
        out[0..4].copy_from_slice(&MAGIC);
        out[4..8].copy_from_slice(&self.seq.to_le_bytes());
        out[8..12].copy_from_slice(&self.width.to_le_bytes());
        out[12..16].copy_from_slice(&self.height.to_le_bytes());
        out[16..20].copy_from_slice(&FORMAT_RGBA8.to_le_bytes());
        out[20..24].copy_from_slice(&self.bytes.to_le_bytes());
        out
    }

    /// Reads a header, or says why those bytes are not one.
    ///
    /// # Errors
    /// When the magic is wrong or the length disagrees with the dimensions —
    /// both of which mean the stream is out of step, and continuing would
    /// paint one frame's pixels with another's size.
    pub fn decode(raw: &[u8; HEADER_BYTES]) -> Result<Self> {
        let take = |at: usize| -> u32 {
            let mut four = [0_u8; 4];
            four.copy_from_slice(raw.get(at..at + 4).unwrap_or(&[0; 4]));
            u32::from_le_bytes(four)
        };
        if raw.get(0..4) != Some(&MAGIC[..]) {
            bail!("not a kirie preview frame");
        }
        if take(16) != FORMAT_RGBA8 {
            bail!("unknown pixel format {}", take(16));
        }
        let header = Self {
            seq: take(4),
            width: take(8),
            height: take(12),
            bytes: take(20),
        };
        let expected = u64::from(header.width) * u64::from(header.height) * 4;
        if u64::from(header.bytes) != expected {
            bail!(
                "frame says {} bytes for {}x{}, which needs {expected}",
                header.bytes,
                header.width,
                header.height
            );
        }
        Ok(header)
    }
}

/// What a client asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Set one property and rebuild.
    Property {
        /// The property's name.
        key: String,
        /// Its new value, verbatim to the end of the line.
        value: String,
    },
    /// Preview a different wallpaper.
    Background(PathBuf),
    /// Render this many frames a second.
    Fps(u32),
    /// Stop rendering, keep the connection.
    Pause,
    /// Start again.
    Resume,
    /// Close.
    Quit,
}

/// Reads one line of the client's grammar.
///
/// Unknown lines are `None` rather than an error: a newer client talking to an
/// older engine should lose the feature it asked for, not the connection.
#[must_use]
pub fn parse(line: &str) -> Option<Command> {
    let line = line.trim_end_matches(['\r', '\n']);
    let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
    match verb {
        "property" => {
            // The value is the rest of the line, so `0.5 0.25 1` needs no
            // quoting — the same rule the control socket follows.
            let (key, value) = rest.split_once(' ')?;
            (!key.is_empty()).then(|| Command::Property {
                key: key.to_owned(),
                value: value.to_owned(),
            })
        }
        "bg" => (!rest.is_empty()).then(|| Command::Background(PathBuf::from(rest))),
        "fps" => rest.parse().ok().map(Command::Fps),
        "pause" => Some(Command::Pause),
        "resume" => Some(Command::Resume),
        "quit" => Some(Command::Quit),
        _ => None,
    }
}

/// How fast to render when nobody says.
const DEFAULT_FPS: u32 = 30;

/// How long to wait for a client before giving up and exiting.
///
/// A preview server is started by an editor and belongs to it. If that editor
/// is killed rather than closed, nothing runs its cleanup — and without this
/// the renderer outlives it, holding a wallpaper's textures for a window that
/// no longer exists. Measured on this machine: 876 MB, indefinitely.
const IDLE_EXIT: Duration = Duration::from_secs(30);

/// The widest a preview is rendered by default.
///
/// A preview is a panel, not a wallpaper: 960 across is more than any of them
/// are shown at, and a frame is 2 MB rather than the 3.7 MB of 1280.
const DEFAULT_EDGE: u32 = 960;

/// Run `kirie preview`.
///
/// Serves one client at a time: a preview belongs to a window, and a second
/// one would double the render cost to show the same thing twice.
///
/// # Errors
/// When the socket cannot be bound, the wallpaper cannot be resolved, or the
/// GPU cannot be brought up.
pub fn run(socket: &Path, background: &Path, fps: Option<u32>, edge: Option<u32>) -> Result<()> {
    // Bound before anything slow, so a client that starts us can connect
    // immediately rather than racing the first scene build.
    let _ = std::fs::remove_file(socket);
    let listener =
        UnixListener::bind(socket).with_context(|| format!("bind preview socket {}", socket.display()))?;
    tracing::info!(socket = %socket.display(), "preview listening");

    let requested_fps = fps.unwrap_or(DEFAULT_FPS).clamp(1, 120);
    let edge = edge.unwrap_or(DEFAULT_EDGE).clamp(64, 3840);

    // Built on the first client and kept for every one after it. The GPU
    // device must outlive them all: the driver pipeline cache is attached to
    // the first device made in the process, so a second device would create
    // pipelines against a cache that no longer exists — which panics inside
    // wgpu rather than failing politely. Reusing it is also what makes a
    // reconnect instant instead of another engine start.
    let mut engine: Option<crate::preview_render::Engine> = None;

    // Non-blocking so the accept loop can give up: a blocking accept would
    // wait for a client that is never coming.
    listener
        .set_nonblocking(true)
        .context("preview socket nonblocking")?;

    let mut waiting_since = Instant::now();
    loop {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if waiting_since.elapsed() >= IDLE_EXIT {
                    tracing::info!("no preview client; exiting");
                    let _ = std::fs::remove_file(socket);
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(error) => {
                tracing::warn!(%error, "preview accept failed");
                continue;
            }
        };
        // Serving is blocking, and the client needs it that way.
        stream.set_nonblocking(false).context("preview client blocking")?;
        // One client at a time, and a client going away is not an error: the
        // window closed.
        if let Err(error) = serve(stream, background, requested_fps, edge, &mut engine) {
            tracing::info!(%error, "preview client finished");
        }
        // Nobody is watching, so nothing needs to be resident. The next client
        // pays a rebuild — about a second — rather than this process holding a
        // wallpaper's textures until it is killed.
        if let Some(engine) = engine.as_mut() {
            engine.release_scene();
        }
        waiting_since = Instant::now();
    }
}

/// Serve one connected client until it leaves.
fn serve(
    stream: UnixStream,
    background: &Path,
    fps: u32,
    edge: u32,
    engine: &mut Option<crate::preview_render::Engine>,
) -> Result<()> {
    let commands = read_commands(stream.try_clone().context("clone preview socket")?);
    let mut session = Session::new(background, edge, fps)?;
    session.pump(stream, &commands, engine)
}

/// Reads client lines on their own thread.
///
/// A reader on the render thread would either block the frames or need the
/// socket to be non-blocking, and the render loop has enough to do.
fn read_commands(stream: UnixStream) -> Receiver<Command> {
    let (sender, receiver) = channel();
    std::thread::Builder::new()
        .name("kirie-preview-cmd".to_owned())
        .spawn(move || {
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { break };
                if let Some(command) = parse(&line)
                    && sender.send(command).is_err()
                {
                    break;
                }
            }
        })
        .ok();
    receiver
}

/// One wallpaper being rendered, and the state a client can change.
struct Session {
    background: PathBuf,
    edge: u32,
    fps: u32,
    paused: bool,
    /// Property overrides, in the order they were set.
    ///
    /// A list rather than a map: the renderer takes pairs, and a preview never
    /// has enough of them for the difference to matter.
    properties: Vec<(String, String)>,
}

impl Session {
    /// Start a session on a wallpaper.
    fn new(background: &Path, edge: u32, fps: u32) -> Result<Self> {
        Ok(Self {
            background: background.to_owned(),
            edge,
            fps,
            paused: false,
            properties: Vec::new(),
        })
    }

    /// Records one command. Returns whether the renderer must be rebuilt.
    fn apply(&mut self, command: Command) -> Rebuild {
        match command {
            Command::Property { key, value } => {
                // Setting the same key twice replaces it; the renderer is
                // handed the final value, not the history.
                if let Some(existing) = self.properties.iter_mut().find(|(existing, _)| *existing == key) {
                    existing.1 = value;
                } else {
                    self.properties.push((key, value));
                }
                Rebuild::Yes
            }
            Command::Background(path) => {
                self.background = path;
                // A different wallpaper has different properties; keeping the
                // old overrides would apply one scene's names to another's.
                self.properties.clear();
                Rebuild::Yes
            }
            Command::Fps(fps) => {
                self.fps = fps.clamp(1, 120);
                Rebuild::No
            }
            Command::Pause => {
                self.paused = true;
                Rebuild::No
            }
            Command::Resume => {
                self.paused = false;
                Rebuild::No
            }
            Command::Quit => Rebuild::Stop,
        }
    }

    /// Render and stream until the client leaves.
    fn pump(
        &mut self,
        mut stream: UnixStream,
        commands: &Receiver<Command>,
        engine: &mut Option<crate::preview_render::Engine>,
    ) -> Result<()> {
        let engine = match engine {
            Some(engine) => {
                // A returning client may want something else on the device
                // that is already up.
                engine.rebuild(&self.background, self.edge, &self.properties)?;
                engine
            }
            slot => slot.insert(crate::preview_render::Engine::new(&self.background, self.edge)?),
        };
        let mut seq: u32 = 0;

        loop {
            let mut rebuild = false;
            loop {
                match commands.try_recv() {
                    Ok(command) => match self.apply(command) {
                        Rebuild::Yes => rebuild = true,
                        Rebuild::No => {}
                        Rebuild::Stop => return Ok(()),
                    },
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return Ok(()),
                }
            }

            if rebuild {
                // Every queued edit lands in one rebuild: a slider drag sends
                // a command per frame, and rebuilding per command would fall
                // further behind the longer it lasts.
                engine.rebuild(&self.background, self.edge, &self.properties)?;
            }

            if self.paused {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }

            let started = Instant::now();
            let (width, height, pixels) = engine.frame()?;
            seq = seq.saturating_add(1);

            let header = FrameHeader {
                seq,
                width,
                height,
                bytes: u32::try_from(pixels.len()).unwrap_or(0),
            };
            // A write failure is the client having closed, which is how a
            // preview normally ends.
            if stream.write_all(&header.encode()).is_err() || stream.write_all(pixels).is_err() {
                return Ok(());
            }

            let budget = Duration::from_secs_f64(1.0 / f64::from(self.fps));
            if let Some(spare) = budget.checked_sub(started.elapsed()) {
                std::thread::sleep(spare);
            }
        }
    }
}

/// What a command means for the renderer.
enum Rebuild {
    /// Rebuild before the next frame.
    Yes,
    /// Nothing to rebuild.
    No,
    /// The client is done.
    Stop,
}

/// Reports a preview socket path that cannot be used before anything is built.
///
/// # Errors
/// When the parent directory is missing, which is the common way a path typed
/// by hand fails.
pub fn check_socket(socket: &Path) -> Result<()> {
    let parent = socket
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if parent.is_dir() {
        return Ok(());
    }
    Err(anyhow!("{} is not a directory", parent.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_round_trips() {
        let header = FrameHeader {
            seq: 7,
            width: 960,
            height: 540,
            bytes: 960 * 540 * 4,
        };
        let decoded = FrameHeader::decode(&header.encode()).expect("decode");
        assert_eq!(decoded, header);
    }

    #[test]
    fn a_header_whose_length_disagrees_with_its_size_is_refused() {
        // Painting one frame's pixels at another frame's size is the failure
        // this exists to make impossible.
        let mut raw = FrameHeader {
            seq: 1,
            width: 4,
            height: 4,
            bytes: 64,
        }
        .encode();
        raw[20] = 1;
        assert!(FrameHeader::decode(&raw).is_err());
    }

    #[test]
    fn anything_that_is_not_a_frame_is_refused() {
        assert!(FrameHeader::decode(&[0; HEADER_BYTES]).is_err());
    }

    #[test]
    fn a_property_value_is_the_rest_of_the_line() {
        // A colour is three numbers separated by spaces, and quoting it would
        // be a second grammar for one field.
        assert_eq!(
            parse("property schemecolor 0.5 0.25 1"),
            Some(Command::Property {
                key: "schemecolor".to_owned(),
                value: "0.5 0.25 1".to_owned(),
            })
        );
    }

    #[test]
    fn a_path_with_spaces_survives() {
        assert_eq!(
            parse("bg /home/a/My Wallpapers/123\n"),
            Some(Command::Background(PathBuf::from("/home/a/My Wallpapers/123")))
        );
    }

    #[test]
    fn an_unknown_line_is_ignored_rather_than_fatal() {
        // A newer client should lose the feature it asked for, not the
        // connection it asked over.
        assert_eq!(parse("teleport 3"), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("property onlyakey"), None);
    }

    #[test]
    fn setting_a_property_twice_keeps_the_last_value() {
        let mut session = Session::new(Path::new("/tmp"), 960, 30).expect("session");
        for value in ["1", "2"] {
            let _ = session.apply(Command::Property {
                key: "speed".to_owned(),
                value: value.to_owned(),
            });
        }
        assert_eq!(session.properties, vec![("speed".to_owned(), "2".to_owned())]);
    }

    #[test]
    fn changing_wallpaper_drops_the_previous_overrides() {
        // Property names belong to a scene; carrying them across would apply
        // one wallpaper's settings to another's keys.
        let mut session = Session::new(Path::new("/tmp/a"), 960, 30).expect("session");
        let _ = session.apply(Command::Property {
            key: "speed".to_owned(),
            value: "2".to_owned(),
        });
        let _ = session.apply(Command::Background(PathBuf::from("/tmp/b")));
        assert!(session.properties.is_empty());
    }

    #[test]
    fn the_frame_rate_stays_in_a_range_that_can_be_served() {
        let mut session = Session::new(Path::new("/tmp"), 960, 30).expect("session");
        let _ = session.apply(Command::Fps(0));
        assert_eq!(session.fps, 1);
        let _ = session.apply(Command::Fps(10_000));
        assert_eq!(session.fps, 120);
    }
}
