use std::io::{ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use std::{env, fs, process, thread};

use crossbeam_channel::{Receiver, unbounded};
use kirie_ipc::{
    ClampMode, Command, CommandOutcome, ControlSocket, IpcEvent, ScalingMode, ScreenStatus, SetOption,
    StatusSnapshot,
};

const LIVE_BG: &str = "/home/aiko/.local/share/Steam/steamapps/workshop/content/431960/3047596375";

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = env::temp_dir().join(format!("kirie-ipc-{}-{name}", process::id()));
        fs::create_dir_all(&dir).expect("create tempdir");
        Self(dir)
    }
    fn sock(&self) -> PathBuf {
        self.0.join("ctl.sock")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct MockApp {
    screens: Vec<ScreenStatus>,
    on_command: Box<dyn FnMut(&Command) -> CommandOutcome + Send>,
    workshop_delay: Duration,
}

impl MockApp {
    fn doc_semantics() -> Self {
        Self {
            screens: vec![ScreenStatus {
                screen: "HDMI-A-1".into(),
                bg: Some(PathBuf::from(LIVE_BG)),
            }],
            on_command: Box::new(|cmd| match cmd {
                Command::Bg { path, .. } if path.starts_with("/bad") => CommandOutcome::Error,
                Command::Property { key, .. } if key == "nosuchkey123" => CommandOutcome::Error,
                Command::Scaling { screen, .. } | Command::Clamp { screen, .. } if screen != "HDMI-A-1" => {
                    CommandOutcome::Error
                }
                Command::Screenshot { path } if path.as_os_str().is_empty() => CommandOutcome::Error,
                _ => CommandOutcome::Ok,
            }),
            workshop_delay: Duration::ZERO,
        }
    }

    fn spawn(mut self, rx: Receiver<IpcEvent>) -> (JoinHandle<()>, Receiver<Command>) {
        let (cap_tx, cap_rx) = unbounded();
        let handle = thread::spawn(move || {
            let mut speed = 1.0f32;
            let mut props: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
            for event in rx {
                match event {
                    IpcEvent::List { reply } => {
                        let _ = reply.send("[]".to_owned());
                    }
                    IpcEvent::Workshop { request, reply } => {
                        let delay = self.workshop_delay;
                        thread::spawn(move || {
                            thread::sleep(delay);
                            let _ = reply.send(format!(r#"{{"request":"{request:?}"}}"#));
                        });
                    }
                    IpcEvent::Status { reply } => {
                        let _ = reply.send(StatusSnapshot {
                            speed,
                            screens: self.screens.clone(),
                        });
                    }
                    IpcEvent::GetProperties { screen: _, reply } => {
                        let body: String = props
                            .iter()
                            .map(|(k, v)| format!(r#"{{"key":"{k}","value":"{v}"}}"#))
                            .collect::<Vec<_>>()
                            .join(",");
                        let _ = reply.send(format!("[{body}]"));
                    }
                    IpcEvent::Command { command, reply } => {
                        if let Command::Speed(v) = command {
                            speed = v;
                        }
                        if let Command::Property { key, value, .. } = &command {
                            props.insert(key.clone(), value.clone());
                        }
                        let outcome = (self.on_command)(&command);
                        let _ = cap_tx.send(command);
                        let _ = reply.send(outcome);
                    }
                }
            }
        });
        (handle, cap_rx)
    }
}

struct Server {
    _dir: TempDir,
    sock: PathBuf,
    server: ControlSocket,
    captured: Receiver<Command>,
    app: Option<JoinHandle<()>>,
}

impl Server {
    fn start(name: &str, mock: MockApp) -> Self {
        let dir = TempDir::new(name);
        let sock = dir.sock();
        let (tx, rx) = unbounded();
        let server = ControlSocket::bind(&sock, tx).expect("bind control socket");
        let (app, captured) = mock.spawn(rx);
        Self {
            _dir: dir,
            sock,
            server,
            captured,
            app: Some(app),
        }
    }

    fn request(&self, bytes: &[u8]) -> Vec<u8> {
        request_at(&self.sock, bytes)
    }

    fn captured(&self) -> Command {
        self.captured
            .recv_timeout(Duration::from_secs(5))
            .expect("mock captured a command")
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.server.shutdown();
        if let Some(app) = self.app.take() {
            let _ = app.join();
        }
    }
}

fn connect(sock: &Path) -> UnixStream {
    let stream = UnixStream::connect(sock).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    stream
}

fn request_at(sock: &Path, bytes: &[u8]) -> Vec<u8> {
    let mut stream = connect(sock);
    stream.write_all(bytes).expect("write request");
    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read response");
    response
}

#[test]
fn ping_pong() {
    let s = Server::start("ping", MockApp::doc_semantics());
    assert_eq!(s.request(b"ping\n"), b"pong\n");
    assert_eq!(s.request(b"ping whatever\n"), b"pong\n");
    assert_eq!(s.request(b"ping\r\n"), b"pong\n");
}

#[test]
fn ping_without_newline_terminated_by_half_close() {
    let s = Server::start("ping-eof", MockApp::doc_semantics());
    let mut stream = connect(&s.sock);
    stream.write_all(b"ping").unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    assert_eq!(response, b"pong\n");
}

#[test]
fn ping_without_newline_terminated_by_read_timeout() {
    let s = Server::start("ping-timeout", MockApp::doc_semantics());
    let mut stream = connect(&s.sock);
    stream.write_all(b"ping").unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    assert_eq!(response, b"pong\n");
}

#[test]
fn second_line_discarded() {
    let s = Server::start("two-lines", MockApp::doc_semantics());
    assert_eq!(s.request(b"ping\nstatus\n"), b"pong\n");
}

#[test]
fn empty_line_gets_zero_response_bytes() {
    let s = Server::start("empty", MockApp::doc_semantics());
    assert_eq!(s.request(b"\n"), b"");
    assert_eq!(s.request(b"  \n"), b"unknown command\n");
}

#[test]
fn unknown_command() {
    let s = Server::start("unknown", MockApp::doc_semantics());
    assert_eq!(s.request(b"frobnicate\n"), b"unknown command\n");
    assert_eq!(s.request(b"PING\n"), b"unknown command\n");
    assert_eq!(s.request(b"quit\n"), b"unknown command\n");
}

#[test]
fn oversized_request_line_is_served() {
    let s = Server::start("oversized", MockApp::doc_semantics());
    let long = "a".repeat(1024 * 1024);
    let mut line = format!("bg HDMI-A-1 /{long}").into_bytes();
    line.push(b'\n');
    assert_eq!(s.request(&line), b"ok\n");
    match s.captured() {
        Command::Bg { screen, path } => {
            assert_eq!(screen, "HDMI-A-1");
            assert_eq!(path.as_os_str().len(), 1 + long.len());
        }
        other => panic!("expected bg, got {other:?}"),
    }
}

#[test]
fn half_open_client_times_out_without_blocking_others() {
    let s = Server::start("half-open", MockApp::doc_semantics());
    let mut idle = connect(&s.sock);
    let started = Instant::now();
    assert_eq!(s.request(b"ping\n"), b"pong\n");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "idle client stalled the server"
    );
    let mut response = Vec::new();
    idle.read_to_end(&mut response).unwrap();
    assert_eq!(response, b"");
}

#[test]
fn late_bytes_after_read_timeout_are_ignored() {
    let s = Server::start("late-bytes", MockApp::doc_semantics());
    let mut stream = connect(&s.sock);
    stream.write_all(b"pi").unwrap();
    thread::sleep(Duration::from_millis(150));
    let _ = stream.write_all(b"ng\n");
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    assert_eq!(response, b"unknown command\n");
}

#[test]
fn status_matches_live_capture_bytes() {
    let s = Server::start("status-live", MockApp::doc_semantics());
    let expected = format!("speed=1\nscreen=HDMI-A-1 bg={LIVE_BG}\n");
    assert_eq!(s.request(b"status\n"), expected.as_bytes());
}

#[test]
fn fixture_file_pairs_are_byte_exact() {
    let fixture = "\
=== request: status ===
speed=1
screen=HDMI-A-1 bg=/home/aiko/.local/share/Steam/steamapps/workshop/content/431960/3047596375
=== request: speed ===
ok
=== request: volume ===
ok
";
    let mut pairs: Vec<(String, String)> = Vec::new();
    for line in fixture.lines() {
        if let Some(name) = line
            .strip_prefix("=== request: ")
            .and_then(|r| r.strip_suffix(" ==="))
        {
            pairs.push((name.to_string(), String::new()));
        } else if let Some((_, response)) = pairs.last_mut() {
            response.push_str(line);
            response.push('\n');
        }
    }
    assert!(!pairs.is_empty(), "fixture parsed no request/response pairs");
    let s = Server::start("fixture", MockApp::doc_semantics());
    for (request, expected) in pairs {
        let actual = s.request(format!("{request}\n").as_bytes());
        assert_eq!(
            actual,
            expected.as_bytes(),
            "response mismatch for fixture request {request:?}"
        );
    }
}

#[test]
fn status_multi_screen_ordering_and_empty_bg() {
    let mock = MockApp {
        workshop_delay: Duration::ZERO,
        screens: vec![
            ScreenStatus {
                screen: "HDMI-A-1".into(),
                bg: Some(PathBuf::from("/w/c")),
            },
            ScreenStatus {
                screen: "DP-2".into(),
                bg: Some(PathBuf::from("/w/has space")),
            },
            ScreenStatus {
                screen: "DP-10".into(),
                bg: None,
            },
        ],
        on_command: Box::new(|_| CommandOutcome::Ok),
    };
    let s = Server::start("status-multi", mock);
    assert_eq!(
        s.request(b"status\n"),
        b"speed=1\nscreen=DP-10 bg=\nscreen=DP-2 bg=/w/has space\nscreen=HDMI-A-1 bg=/w/c\n"
    );
}

#[test]
fn status_reflects_speed_commands() {
    let s = Server::start("status-speed", MockApp::doc_semantics());
    assert_eq!(s.request(b"speed 0.5\n"), b"ok\n");
    let _ = s.captured();
    assert!(s.request(b"status\n").starts_with(b"speed=0.5\n"));
    assert_eq!(s.request(b"speed 0\n"), b"ok\n");
    let _ = s.captured();
    assert!(s.request(b"status\n").starts_with(b"speed=1\n"));
}

#[test]
fn getproperties_reflects_property_overrides_over_the_socket() {
    let s = Server::start("getprops", MockApp::doc_semantics());
    assert_eq!(s.request(b"getproperties\n"), b"[]\n");
    assert_eq!(s.request(b"property HDMI-A-1 bloom true\n"), b"ok\n");
    let _ = s.captured();
    assert_eq!(s.request(b"property HDMI-A-1 outline 0.5 0.25 0.75\n"), b"ok\n");
    let _ = s.captured();
    let body = s.request(b"getproperties HDMI-A-1\n");
    assert_eq!(
        body,
        br#"[{"key":"bloom","value":"true"},{"key":"outline","value":"0.5 0.25 0.75"}]"#
            .iter()
            .chain(b"\n")
            .copied()
            .collect::<Vec<u8>>()
            .as_slice()
    );
    assert_eq!(body.iter().filter(|&&b| b == b'\n').count(), 1);
    assert_eq!(body.last(), Some(&b'\n'));
}

#[test]
fn getproperties_is_unknown_absent_an_app_arm() {
    let dir = TempDir::new("getprops-dead");
    let sock = dir.sock();
    let (tx, rx) = unbounded();
    drop(rx);
    let _server = ControlSocket::bind(&sock, tx).expect("bind");
    assert_eq!(request_at(&sock, b"getproperties\n"), b"");
}

#[test]
fn bare_speed_and_volume_reply_ok_like_live_capture() {
    let s = Server::start("bare-args", MockApp::doc_semantics());
    assert_eq!(s.request(b"speed\n"), b"ok\n");
    assert_eq!(s.captured(), Command::Speed(1.0));
    assert_eq!(s.request(b"volume\n"), b"ok\n");
    assert_eq!(s.captured(), Command::Volume(128));
    assert_eq!(s.request(b"mute\n"), b"ok\n");
    assert_eq!(s.captured(), Command::Mute(false));
}

#[test]
fn volume_is_not_clamped() {
    let s = Server::start("volume-raw", MockApp::doc_semantics());
    assert_eq!(s.request(b"volume 500\n"), b"ok\n");
    assert_eq!(s.captured(), Command::Volume(500));
    assert_eq!(s.request(b"volume -7\n"), b"ok\n");
    assert_eq!(s.captured(), Command::Volume(-7));
    assert_eq!(s.request(b"volume abc\n"), b"ok\n");
    assert_eq!(s.captured(), Command::Volume(0));
}

#[test]
fn mute_nonzero_semantics() {
    let s = Server::start("mute", MockApp::doc_semantics());
    assert_eq!(s.request(b"mute 1\n"), b"ok\n");
    assert_eq!(s.captured(), Command::Mute(true));
    assert_eq!(s.request(b"mute 0\n"), b"ok\n");
    assert_eq!(s.captured(), Command::Mute(false));
    assert_eq!(s.request(b"mute 2\n"), b"ok\n");
    assert_eq!(s.captured(), Command::Mute(true));
}

#[test]
fn set_recognized_keys_ok_unknown_key_error() {
    let s = Server::start("set", MockApp::doc_semantics());
    assert_eq!(s.request(b"set fps 30\n"), b"ok\n");
    assert_eq!(s.captured(), Command::Set(SetOption::Fps(30)));
    assert_eq!(s.request(b"set renderscale 5\n"), b"ok\n");
    assert_eq!(s.captured(), Command::Set(SetOption::RenderScale(2.0)));
    assert_eq!(s.request(b"set audiodevice default\n"), b"ok\n");
    assert_eq!(s.captured(), Command::Set(SetOption::AudioDevice(String::new())));
    assert_eq!(s.request(b"set disablemouse TRUE\n"), b"ok\n");
    assert_eq!(s.captured(), Command::Set(SetOption::DisableMouse(false)));
    assert_eq!(s.request(b"set bogus 1\n"), b"error\n");
    assert_eq!(s.request(b"set\n"), b"error\n");
    assert!(s.captured.is_empty(), "rejected set leaked to the app");
}

#[test]
fn preload_replies_ok_even_when_the_app_fails() {
    let mock = MockApp {
        workshop_delay: Duration::ZERO,
        screens: vec![],
        on_command: Box::new(|_| CommandOutcome::Error),
    };
    let s = Server::start("preload", mock);
    assert_eq!(s.request(b"preload /definitely/not/a/wallpaper\n"), b"ok\n");
    assert_eq!(
        s.captured(),
        Command::Preload {
            path: PathBuf::from("/definitely/not/a/wallpaper")
        }
    );
}

#[test]
fn bg_ok_and_error_paths() {
    let s = Server::start("bg", MockApp::doc_semantics());
    let line = format!("bg HDMI-A-1 {LIVE_BG}\n");
    assert_eq!(s.request(line.as_bytes()), b"ok\n");
    assert_eq!(
        s.captured(),
        Command::Bg {
            screen: "HDMI-A-1".into(),
            path: PathBuf::from(LIVE_BG)
        }
    );
    assert_eq!(s.request(b"bg HDMI-A-1 /bad/dir\n"), b"error\n");
    let _ = s.captured();
    assert_eq!(s.request(b"bg BOGUS /w/fine\n"), b"ok\n");
    assert_eq!(
        s.captured(),
        Command::Bg {
            screen: "BOGUS".into(),
            path: PathBuf::from("/w/fine")
        }
    );
    assert_eq!(s.request(b"bg HDMI-A-1 /w/dir with spaces\n"), b"ok\n");
    assert_eq!(
        s.captured(),
        Command::Bg {
            screen: "HDMI-A-1".into(),
            path: PathBuf::from("/w/dir with spaces")
        }
    );
}

#[test]
fn property_ok_error_and_value_fidelity() {
    let s = Server::start("property", MockApp::doc_semantics());
    assert_eq!(s.request(b"property HDMI-A-1 bloom true\n"), b"ok\n");
    assert_eq!(
        s.captured(),
        Command::Property {
            screen: "HDMI-A-1".into(),
            key: "bloom".into(),
            value: "true".into()
        }
    );
    assert_eq!(
        s.request(b"property HDMI-A-1 outline 0.36585 0.04268 0.43902\n"),
        b"ok\n"
    );
    assert_eq!(
        s.captured(),
        Command::Property {
            screen: "HDMI-A-1".into(),
            key: "outline".into(),
            value: "0.36585 0.04268 0.43902".into(),
        }
    );
    assert_eq!(s.request(b"property HDMI-A-1 nosuchkey123 1\n"), b"error\n");
}

#[test]
fn scaling_and_clamp_modes_and_errors() {
    let s = Server::start("scaling", MockApp::doc_semantics());
    assert_eq!(s.request(b"scaling HDMI-A-1 fill\n"), b"ok\n");
    assert_eq!(
        s.captured(),
        Command::Scaling {
            screen: "HDMI-A-1".into(),
            mode: ScalingMode::Fill
        }
    );
    assert_eq!(s.request(b"scaling HDMI-A-1 bogusmode\n"), b"error\n");
    assert!(s.captured.is_empty(), "invalid scaling mode leaked to the app");
    assert_eq!(s.request(b"scaling DP-9 fit\n"), b"error\n");
    assert_eq!(
        s.captured(),
        Command::Scaling {
            screen: "DP-9".into(),
            mode: ScalingMode::Fit
        }
    );

    assert_eq!(s.request(b"clamp HDMI-A-1 border\n"), b"ok\n");
    assert_eq!(
        s.captured(),
        Command::Clamp {
            screen: "HDMI-A-1".into(),
            mode: ClampMode::Border
        }
    );
    assert_eq!(s.request(b"clamp HDMI-A-1 nope\n"), b"error\n");
    assert!(s.captured.is_empty(), "invalid clamp mode leaked to the app");
}

#[test]
fn screenshot_ok_and_empty_path_error() {
    let s = Server::start("screenshot", MockApp::doc_semantics());
    assert_eq!(s.request(b"screenshot /tmp/kirie test.png\n"), b"ok\n");
    assert_eq!(
        s.captured(),
        Command::Screenshot {
            path: PathBuf::from("/tmp/kirie test.png")
        }
    );
    assert_eq!(s.request(b"screenshot\n"), b"error\n");
}

#[test]
fn stale_socket_file_is_unlinked_on_bind() {
    let dir = TempDir::new("stale");
    let sock = dir.sock();
    fs::write(&sock, b"stale").unwrap();
    let (tx, rx) = unbounded();
    let server = ControlSocket::bind(&sock, tx).expect("bind over stale file");
    let (app, _cap) = MockApp::doc_semantics().spawn(rx);
    assert_eq!(request_at(&sock, b"ping\n"), b"pong\n");
    drop(server);
    let _ = app.join();
    assert!(!sock.exists(), "socket file left behind after shutdown");
}

#[test]
fn app_gone_yields_dead_engine_signal() {
    let dir = TempDir::new("app-gone");
    let sock = dir.sock();
    let (tx, rx) = unbounded();
    drop(rx);
    let _server = ControlSocket::bind(&sock, tx).expect("bind");
    assert_eq!(request_at(&sock, b"ping\n"), b"pong\n");
    assert_eq!(request_at(&sock, b"speed 1\n"), b"");
    assert_eq!(request_at(&sock, b"status\n"), b"");
}

#[test]
fn concurrent_clients_are_all_served() {
    let s = Server::start("concurrent", MockApp::doc_semantics());
    let sock = s.sock.clone();
    let handles: Vec<_> = (0..10)
        .map(|i| {
            let sock = sock.clone();
            thread::spawn(move || request_at(&sock, format!("property HDMI-A-1 fov {i}\n").as_bytes()))
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().unwrap(), b"ok\n");
    }
    for _ in 0..10 {
        let _ = s.captured();
    }
}

#[test]
fn non_utf8_bg_path_reaches_app_byte_exact() {
    let s = Server::start("non-utf8", MockApp::doc_semantics());
    let mut line = b"bg HDMI-A-1 /weird/\xff\xfe/dir".to_vec();
    line.push(b'\n');
    assert_eq!(s.request(&line), b"ok\n");
    match s.captured() {
        Command::Bg { path, .. } => assert_eq!(path.as_os_str().as_bytes(), b"/weird/\xff\xfe/dir"),
        other => panic!("expected bg, got {other:?}"),
    }
}

#[test]
fn round_trip_every_command_variant_over_the_socket() {
    let s = Server::start("roundtrip", MockApp::doc_semantics());
    let all = [
        Command::Speed(0.5),
        Command::Volume(64),
        Command::Mute(true),
        Command::Set(SetOption::Fps(30)),
        Command::Set(SetOption::NoAutomute(true)),
        Command::Set(SetOption::DisableMouse(false)),
        Command::Set(SetOption::DisableParallax(true)),
        Command::Set(SetOption::NoFullscreenPause(false)),
        Command::Set(SetOption::RenderScale(1.06)),
        Command::Set(SetOption::AudioDevice("alsa_output.pci 0000_00.analog".into())),
        Command::Bg {
            screen: "HDMI-A-1".into(),
            path: PathBuf::from("/path/with spaces/dir"),
        },
        Command::Preload {
            path: PathBuf::from("/w/431960/3047596375"),
        },
        Command::Property {
            screen: "HDMI-A-1".into(),
            key: "outline".into(),
            value: "0.36585 0.04268 0.43902".into(),
        },
        Command::Scaling {
            screen: "HDMI-A-1".into(),
            mode: ScalingMode::Stretch,
        },
        Command::Clamp {
            screen: "HDMI-A-1".into(),
            mode: ClampMode::Repeat,
        },
        Command::Screenshot {
            path: PathBuf::from("/tmp/shot.png"),
        },
    ];
    for cmd in all {
        let mut line = cmd.to_request_line();
        line.push(b'\n');
        let response = s.request(&line);
        assert_eq!(response, b"ok\n", "unexpected response for {cmd:?}");
        assert_eq!(s.captured(), cmd, "command mangled in transit");
    }
}

#[test]
fn read_timeout_close_is_observable_quickly() {
    let s = Server::start("timeout-latency", MockApp::doc_semantics());
    let mut stream = connect(&s.sock);
    let started = Instant::now();
    let mut sink = Vec::new();
    stream.read_to_end(&mut sink).unwrap();
    let elapsed = started.elapsed();
    assert_eq!(sink, b"");
    assert!(
        elapsed < Duration::from_millis(1500),
        "silent client held open for {elapsed:?}"
    );
}

#[test]
fn shutdown_is_idempotent_and_unbinds() {
    let dir = TempDir::new("shutdown");
    let sock = dir.sock();
    let (tx, rx) = unbounded();
    let mut server = ControlSocket::bind(&sock, tx).expect("bind");
    assert_eq!(server.path(), sock.as_path());
    let (app, _cap) = MockApp::doc_semantics().spawn(rx);
    assert_eq!(request_at(&sock, b"ping\n"), b"pong\n");
    server.shutdown();
    server.shutdown();
    let _ = app.join();
    assert!(!sock.exists());
    match UnixStream::connect(&sock) {
        Err(e) => assert!(
            matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused),
            "unexpected error kind {e:?}"
        ),
        Ok(_) => panic!("socket still accepting after shutdown"),
    }
}

#[test]
fn workshop_verbs_parse_into_typed_requests() {
    let s = Server::start("workshop-parse", MockApp::doc_semantics());

    let reply = String::from_utf8(s.request(b"workshop search tag=Scene text=blue sky\n")).unwrap();
    assert!(reply.ends_with('\n'), "framed like list/getproperties");
    assert!(
        reply.contains(r#"Search("tag=Scene text=blue sky")"#),
        "the query reaches the app unparsed: {reply}"
    );

    let reply = String::from_utf8(s.request(b"workshop state 1388331347\n")).unwrap();
    assert!(reply.contains(r#"State("1388331347")"#), "{reply}");

    let reply = String::from_utf8(s.request(b"workshop subscribe 42\n")).unwrap();
    assert!(reply.contains(r#"Subscribe("42")"#), "{reply}");

    let reply = String::from_utf8(s.request(b"workshop job 7\n")).unwrap();
    assert!(reply.contains("Job(7)"), "{reply}");

    assert_eq!(s.request(b"workshop rummage\n"), b"unknown command\n");
    assert_eq!(s.request(b"workshop\n"), b"unknown command\n");
    assert_eq!(s.request(b"workshop state\n"), b"error\n");
    assert_eq!(s.request(b"workshop job notanumber\n"), b"error\n");
}

#[test]
fn a_slow_workshop_request_does_not_stall_other_clients() {
    let mut mock = MockApp::doc_semantics();
    mock.workshop_delay = Duration::from_millis(600);
    let s = Server::start("workshop-slow", mock);

    let sock = s.sock.clone();
    let slow = thread::spawn(move || request_at(&sock, b"workshop search sort=popular\n"));
    thread::sleep(Duration::from_millis(100));

    let started = Instant::now();
    assert_eq!(s.request(b"ping\n"), b"pong\n");
    let waited = started.elapsed();
    assert!(
        waited < Duration::from_millis(400),
        "ping waited {waited:?} behind a slow workshop request"
    );

    let reply = slow.join().expect("the slow request still answers");
    assert!(String::from_utf8_lossy(&reply).contains("Search("));
}
