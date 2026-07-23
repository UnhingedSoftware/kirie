//! `kirie-webviewhost` — the out-of-process webview (webkit2gtk) wallpaper
//! host.
//!
//! webkit has no off-screen rendering path, so instead of publishing frames
//! (the CEF `kirie-webhost` model) this host presents the wallpaper *itself*:
//! it owns a GTK window promoted to the compositor's **background layer** via
//! gtk-layer-shell (anchored to all edges, exclusive zone -1, namespace
//! containing `wallpaperengine` for the daemon watchdog) and lets webkit
//! render straight into it. The engine's own layer surface sits beneath and
//! stays black.
//!
//! The window's Wayland input region is set EMPTY so every pointer event
//! passes through to whatever the compositor finds beneath (desktop
//! right-click menus keep working); pointer interactivity for pages is
//! delivered as synthetic DOM events via the `pointer` command instead.
//!
//! Protocol: stdin lines `props <json>` / `mute <0|1>` /
//! `pointer <x> <y> <l> <r>` / `resize <w> <h>` (informational; the layer
//! window is output-anchored) / `quit`; stdout `ready` once the page is up.
//! Engine death = stdin EOF = quit. See `kirie_web::viewhost` (the engine
//! side) for the pairing client.

use gtk::prelude::*;
use gtk_layer_shell::LayerShell;

use kirie_web::WebSize;
use kirie_web::webview::WebviewBackend;

fn arg(name: &str) -> Option<String> {
    let mut args = std::env::args();
    while let Some(a) = args.next() {
        if a == name {
            return args.next();
        }
    }
    None
}

fn main() {
    // Child logs to stderr, which the engine inherits into its own log.
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

    // On explicit-sync compositors (Hyprland + NVIDIA) webkit's DMABUF
    // renderer commits buffers without a drm-syncobj acquire timeline and the
    // compositor kills the connection: wl_display error
    // `wp_linux_drm_syncobj_surface_v1: "Missing acquire timeline"` →
    // "Gdk Error 71 (Protocol error)". Default webkit to its shared-memory
    // renderer (same workaround Tauri ships); setting the variable to any
    // value beforehand (e.g. "0") overrides this.
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        // SAFETY: pre-gtk::init, before any thread is spawned — the process
        // is still single-threaded, which is the set_var requirement.
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
    // Must contain "wallpaperengine": the wallpaper daemon's watchdog decides
    // a monitor still has a live wallpaper by grepping layer namespaces.
    // (Keyboard interactivity is left at its default, None.)
    window.set_namespace("linux-wallpaperengine-webview");

    // Pick the output whose logical size matches the requested one when there
    // are several (single-monitor setups always match). TODO: match by output
    // name once the engine forwards connector names (gdk3 has no connector
    // API; geometry is the best available key).
    if let Some(display) = gtk::gdk::Display::default()
        && display.n_monitors() > 1
    {
        for i in 0..display.n_monitors() {
            if let Some(mon) = display.monitor(i) {
                let g = mon.geometry();
                if g.width() as u32 == width && g.height() as u32 == height {
                    window.set_monitor(&mon);
                    break;
                }
            }
        }
    }

    let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
    window.add(&container);
    window.set_default_size(width as i32, height as i32);
    window.show_all();

    // Empty input region: the wallpaper must never swallow pointer input
    // meant for the desktop (menus, widget layers). On Wayland this sets the
    // wl_surface input region of the toplevel.
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

    // Re-apply the empty input region whenever the surface is (re)mapped —
    // a remap can reset it.
    {
        let win = window.clone();
        window.connect_map_event(move |_, _| {
            if let Some(gdk_window) = win.window() {
                gdk_window.input_shape_combine_region(&gtk::cairo::Region::create(), 0, 0);
            }
            gtk::glib::Propagation::Proceed
        });
    }

    // stdin command reader → mpsc → polled on the GTK main loop (the backend
    // is GTK-main-thread-bound). Commands are rare; a 15 ms poll costs
    // nothing and avoids glib's deprecated channel API.
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
        // Engine hung up (crash?) — no reason to outlive it.
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
                            .send_pointer(kirie_web::PointerState { x, y, left, right });
                    }
                    Some("resize") => {
                        let w = p.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                        let h = p.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                        if w > 0 && h > 0 {
                            backend.borrow_mut().resize(WebSize { width: w, height: h });
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

    // Exit without running webkit teardown: the engine kills or quits this
    // process to tear the wallpaper down, and the kernel + webkit's
    // parent-death handling reclaim everything (same rationale as
    // kirie-webhost's CEF exit path).
    std::mem::forget(backend);
    std::process::exit(0);
}
