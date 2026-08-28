use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub seq: u32,
    pub width: u32,
    pub height: u32,
    pub bytes: u32,
}

pub const MAGIC: [u8; 4] = *b"KPV1";

const FORMAT_RGBA8: u32 = 0;

pub const HEADER_BYTES: usize = 24;

impl FrameHeader {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Property { key: String, value: String },
    Background(PathBuf),
    Fps(u32),
    Pause,
    Resume,
    Quit,
}

#[must_use]
pub fn parse(line: &str) -> Option<Command> {
    let line = line.trim_end_matches(['\r', '\n']);
    let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
    match verb {
        "property" => {
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

const DEFAULT_FPS: u32 = 30;

const IDLE_EXIT: Duration = Duration::from_secs(30);

const DEFAULT_EDGE: u32 = 960;

pub fn run(socket: &Path, background: &Path, fps: Option<u32>, edge: Option<u32>) -> Result<()> {
    let _ = std::fs::remove_file(socket);
    let listener =
        UnixListener::bind(socket).with_context(|| format!("bind preview socket {}", socket.display()))?;
    tracing::info!(socket = %socket.display(), "preview listening");

    let requested_fps = fps.unwrap_or(DEFAULT_FPS).clamp(1, 120);
    let edge = edge.unwrap_or(DEFAULT_EDGE).clamp(64, 3840);

    let mut engine: Option<crate::preview_render::Engine> = None;

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
        stream.set_nonblocking(false).context("preview client blocking")?;
        if let Err(error) = serve(stream, background, requested_fps, edge, &mut engine) {
            tracing::info!(%error, "preview client finished");
        }
        if let Some(engine) = engine.as_mut() {
            engine.release_scene();
        }
        waiting_since = Instant::now();
    }
}

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

struct Session {
    background: PathBuf,
    edge: u32,
    fps: u32,
    paused: bool,
    properties: Vec<(String, String)>,
}

impl Session {
    fn new(background: &Path, edge: u32, fps: u32) -> Result<Self> {
        Ok(Self {
            background: background.to_owned(),
            edge,
            fps,
            paused: false,
            properties: Vec::new(),
        })
    }

    fn apply(&mut self, command: Command) -> Rebuild {
        match command {
            Command::Property { key, value } => {
                if let Some(existing) = self.properties.iter_mut().find(|(existing, _)| *existing == key) {
                    existing.1 = value;
                } else {
                    self.properties.push((key, value));
                }
                Rebuild::Yes
            }
            Command::Background(path) => {
                self.background = path;
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

    fn pump(
        &mut self,
        mut stream: UnixStream,
        commands: &Receiver<Command>,
        engine: &mut Option<crate::preview_render::Engine>,
    ) -> Result<()> {
        let engine = match engine {
            Some(engine) => {
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

enum Rebuild {
    Yes,
    No,
    Stop,
}

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
