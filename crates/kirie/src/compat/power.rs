use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use kirie_platform::{CommandSender, RenderCommand};

const POLL: Duration = Duration::from_secs(10);

pub struct PowerWatch {
    pub cmd_tx: CommandSender,
    pub normal_fps: Option<u32>,
    pub battery_fps: Arc<AtomicU32>,
    pub power_save: Arc<AtomicBool>,
    pub baker: Option<Arc<kirie_bake::BackgroundBaker>>,
    pub stop: Arc<AtomicBool>,
}

pub fn spawn(watch: PowerWatch) -> Option<JoinHandle<()>> {
    if force_override().is_none() && !upower_available() && !sysfs_has_battery() {
        tracing::debug!("no battery present; power watcher not started");
        return None;
    }
    Some(
        std::thread::Builder::new()
            .name("kirie-power".into())
            .spawn(move || run(watch))
            .expect("spawn power watcher"),
    )
}

fn run(watch: PowerWatch) {
    let conn = zbus::blocking::Connection::system().ok();
    let mut applied = false;
    while !watch.stop.load(Ordering::Relaxed) {
        let battery_fps = watch.battery_fps.load(Ordering::Relaxed);
        let want = battery_fps > 0 && on_battery(conn.as_ref());
        if want != applied {
            applied = want;
            watch.power_save.store(want, Ordering::Relaxed);
            if want {
                tracing::info!(fps = battery_fps, "on battery; power profile applied");
                let _ = watch.cmd_tx.send(RenderCommand::SetFps(Some(battery_fps)));
                if let Some(baker) = &watch.baker {
                    baker.pause();
                }
            } else {
                tracing::info!(fps = ?watch.normal_fps, "on AC; power profile reverted");
                let _ = watch.cmd_tx.send(RenderCommand::SetFps(watch.normal_fps));
                if let Some(baker) = &watch.baker {
                    baker.resume();
                }
            }
        }
        std::thread::park_timeout(POLL);
    }
}

fn force_override() -> Option<bool> {
    match std::env::var("KIRIE_FORCE_POWER").ok()?.as_str() {
        "battery" => Some(true),
        "ac" => Some(false),
        _ => None,
    }
}

fn on_battery(conn: Option<&zbus::blocking::Connection>) -> bool {
    if let Some(forced) = force_override() {
        return forced;
    }
    if let Some(conn) = conn
        && let Some(b) = upower_on_battery(conn)
    {
        return b;
    }
    sysfs_on_battery()
}

fn upower_on_battery(conn: &zbus::blocking::Connection) -> Option<bool> {
    let proxy = zbus::blocking::Proxy::new(
        conn,
        "org.freedesktop.UPower",
        "/org/freedesktop/UPower",
        "org.freedesktop.UPower",
    )
    .ok()?;
    proxy.get_property::<bool>("OnBattery").ok()
}

fn upower_available() -> bool {
    zbus::blocking::Connection::system()
        .ok()
        .and_then(|c| upower_on_battery(&c))
        .is_some()
}

fn sysfs_has_battery() -> bool {
    supplies().any(|dir| read_trim(&dir.join("type")).as_deref() == Some("Battery"))
}

fn sysfs_on_battery() -> bool {
    let mut has_battery = false;
    for dir in supplies() {
        match read_trim(&dir.join("type")).as_deref() {
            Some("Mains") | Some("USB") | Some("ADP") => {
                if read_trim(&dir.join("online")).as_deref() == Some("1") {
                    return false;
                }
            }
            Some("Battery") => has_battery = true,
            _ => {}
        }
    }
    has_battery
}

fn supplies() -> impl Iterator<Item = std::path::PathBuf> {
    std::fs::read_dir("/sys/class/power_supply")
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
}

fn read_trim(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_supplies_means_ac() {
        if !sysfs_has_battery() {
            assert!(!sysfs_on_battery());
        }
    }
}
