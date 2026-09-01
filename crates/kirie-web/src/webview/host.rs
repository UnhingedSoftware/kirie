use gtk::prelude::*;
use gtk_layer_shell::LayerShell;

use crate::WebSize;
use crate::webview::WebviewBackend;

fn arg(name: &str) -> Option<String> {
    let mut args = std::env::args();
    while let Some(a) = args.next() {
        if a == name {
            return args.next();
        }
    }
    None
}

pub fn run() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let Some(url) = arg("--url") else {
        eprintln!("kirie-webviewhost: missing --url");
        std::process::exit(2);
    };
    let width: u32 = arg("--width").and_then(|v| v.parse().ok()).unwrap_or(1920);
    let height: u32 = arg("--height").and_then(|v| v.parse().ok()).unwrap_or(1080);

    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        // SAFETY: pre-gtk::init, before any thread is spawned — the process
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }

    if gtk::init().is_err() {
        eprintln!("kirie-webviewhost: gtk init failed (no display?)");
        std::process::exit(1);
    }

    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    window.init_layer_shell();
    window.set_layer(gtk_layer_shell::Layer::Background);
    for edge in [
        gtk_layer_shell::Edge::Top,
        gtk_layer_shell::Edge::Bottom,
        gtk_layer_shell::Edge::Left,
        gtk_layer_shell::Edge::Right,
    ] {
        window.set_anchor(edge, true);
    }
    window.set_exclusive_zone(-1);
    window.set_namespace("linux-wallpaperengine-webview");

    let wanted = arg("--output");
    let at = arg("--x")
        .and_then(|v| v.parse::<i32>().ok())
        .zip(arg("--y").and_then(|v| v.parse::<i32>().ok()));
    if let Some(display) = gtk::gdk::Display::default()
        && display.n_monitors() > 1
    {
        let placed = at.and_then(|(x, y)| {
            (0..display.n_monitors()).find_map(|i| {
                let mon = display.monitor(i)?;
                let g = mon.geometry();
                (g.x() == x && g.y() == y).then_some(mon)
            })
        });
        let named = wanted.as_deref().and_then(|name| {
            (0..display.n_monitors()).find_map(|i| {
                let mon = display.monitor(i)?;
                let model = mon.model().map(|m| m.to_string()).unwrap_or_default();
                model.eq_ignore_ascii_case(name).then_some(mon)
            })
        });
        let picked = placed.or(named).or_else(|| {
            (0..display.n_monitors()).find_map(|i| {
                let mon = display.monitor(i)?;
                let g = mon.geometry();
                (g.width() as u32 == width && g.height() as u32 == height).then_some(mon)
            })
        });
        if let Some(mon) = picked {
            tracing::debug!(
                model = mon.model().map(|m| m.to_string()).unwrap_or_default(),
                wanted = wanted.as_deref().unwrap_or("<any>"),
                "webview host picked a monitor"
            );
            window.set_monitor(&mon);
        }
    }

    let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
    window.add(&container);
    window.set_default_size(width as i32, height as i32);
    window.show_all();

    if let Some(gdk_window) = window.window() {
        gdk_window.input_shape_combine_region(&gtk::cairo::Region::create(), 0, 0);
    }

    let size = WebSize { width, height };
    let backend = match WebviewBackend::with_gtk_container(&url, size, &container, false) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("kirie-webviewhost: webview start failed: {e}");
            std::process::exit(1);
        }
    };
    let backend = std::rc::Rc::new(std::cell::RefCell::new(backend));

    {
        let win = window.clone();
        window.connect_map_event(move |_, _| {
            if let Some(gdk_window) = win.window() {
                gdk_window.input_shape_combine_region(&gtk::cairo::Region::create(), 0, 0);
            }
            gtk::glib::Propagation::Proceed
        });
    }

    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        use std::io::BufRead;
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
        let _ = tx.send("quit".to_owned());
    });

    {
        let backend = backend.clone();
        gtk::glib::timeout_add_local(std::time::Duration::from_millis(15), move || {
            while let Ok(line) = rx.try_recv() {
                let mut p = line.split_whitespace();
                match p.next() {
                    Some("props") => {
                        if let Some(rest) = line.strip_prefix("props ") {
                            backend.borrow_mut().apply_user_properties(rest);
                        }
                    }
                    Some("mute") => backend.borrow_mut().set_muted(p.next() == Some("1")),
                    Some("pointer") => {
                        let x = p.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                        let y = p.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                        let left = p.next() == Some("1");
                        let right = p.next() == Some("1");
                        backend
                            .borrow_mut()
                            .send_pointer(crate::PointerState { x, y, left, right });
                    }
                    Some("resize") => {
                        let w = p.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                        let h = p.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                        if w > 0 && h > 0 {
                            backend.borrow_mut().resize(WebSize { width: w, height: h });
                        }
                    }
                    Some("audio") => {
                        if let Some(rest) = line.strip_prefix("audio ") {
                            let bands = crate::feed::parse_audio_bands(rest);
                            if !bands.is_empty() {
                                backend.borrow_mut().push_audio(&bands);
                            }
                        }
                    }
                    Some("media") => {
                        if let Some(rest) = line.strip_prefix("media ")
                            && let Some((channel, json)) = crate::feed::parse_media_payload(rest)
                        {
                            backend.borrow_mut().push_media(channel, json);
                        }
                    }
                    Some("snap") => {
                        if let Some(path) = line.strip_prefix("snap ") {
                            let r = backend.borrow().snapshot_raw(path);
                            match r {
                                Some((w, h)) => println!("snap ok {w} {h}"),
                                None => println!("snap fail"),
                            }
                            use std::io::Write;
                            let _ = std::io::stdout().flush();
                        }
                    }
                    Some("quit") => {
                        gtk::main_quit();
                        return gtk::glib::ControlFlow::Break;
                    }
                    _ => {}
                }
            }
            gtk::glib::ControlFlow::Continue
        });
    }

    println!("ready");
    use std::io::Write;
    let _ = std::io::stdout().flush();

    gtk::main();

    std::mem::forget(backend);
    std::process::exit(0);
}
