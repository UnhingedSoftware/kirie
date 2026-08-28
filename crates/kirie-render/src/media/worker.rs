use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Duration;

use arc_swap::ArcSwap;
use zbus::zvariant::OwnedValue;

use super::art::{AlbumArt, load_art};
use super::metadata::parse_metadata;
use super::state::{MediaState, PlaybackState};
use super::{MediaStatus, WorkerParams};

const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";

struct ArtCache {
    url: Option<String>,
    art: Option<Arc<AlbumArt>>,
}

impl ArtCache {
    const fn new() -> Self {
        Self { url: None, art: None }
    }

    fn resolve(&mut self, url: Option<&str>) -> Option<Arc<AlbumArt>> {
        if self.url.as_deref() == url {
            return self.art.clone();
        }
        self.url = url.map(str::to_owned);
        self.art = url.and_then(load_art).map(Arc::new);
        self.art.clone()
    }
}

pub(super) fn run(
    shared: Arc<ArcSwap<MediaState>>,
    status: Arc<AtomicU8>,
    shutdown: Arc<AtomicBool>,
    params: WorkerParams,
) {
    let conn = match zbus::blocking::Connection::session() {
        Ok(c) => {
            status.store(MediaStatus::Connected.as_u8(), Ordering::Relaxed);
            c
        }
        Err(e) => {
            tracing::info!(error = %e, "no D-Bus session bus; media state stays empty");
            status.store(MediaStatus::Failed.as_u8(), Ordering::Relaxed);
            shared.store(Arc::new(MediaState::empty()));
            return;
        }
    };

    let mut art_cache = ArtCache::new();

    const IDLE_TICK: Duration = Duration::from_secs(5);
    while !shutdown.load(Ordering::Relaxed) {
        let state = poll_once(&conn, &mut art_cache);
        let idle = !state.available;
        shared.store(Arc::new(state));
        sleep_interruptible(
            &shutdown,
            if idle {
                IDLE_TICK.max(params.tick)
            } else {
                params.tick
            },
        );
    }
}

fn poll_once(conn: &zbus::blocking::Connection, art_cache: &mut ArtCache) -> MediaState {
    let Some(player) = detect_player(conn) else {
        return MediaState::empty();
    };

    let Ok(proxy) = player_proxy(conn, &player) else {
        return MediaState::empty();
    };

    let playback = proxy
        .get_property::<String>("PlaybackStatus")
        .map(|s| PlaybackState::from_mpris(&s))
        .unwrap_or_default();

    let metadata = proxy
        .get_property::<HashMap<String, OwnedValue>>("Metadata")
        .map(|d| parse_metadata(&d))
        .unwrap_or_default();

    let position_us = proxy.get_property::<i64>("Position").unwrap_or(0);

    let art = art_cache.resolve(metadata.art_url.as_deref());

    MediaState {
        available: true,
        player_pid: connection_pid(conn, &player),
        player: Some(player),
        playback,
        metadata,
        position_us,
        art,
    }
}

fn connection_pid(conn: &zbus::blocking::Connection, bus_name: &str) -> Option<u32> {
    let proxy = zbus::blocking::Proxy::new(
        conn,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .ok()?;
    proxy.call("GetConnectionUnixProcessID", &(bus_name)).ok()
}

fn player_proxy<'a>(
    conn: &zbus::blocking::Connection,
    player: &str,
) -> zbus::Result<zbus::blocking::Proxy<'a>> {
    zbus::blocking::Proxy::new(conn, player.to_owned(), OBJECT_PATH, PLAYER_IFACE)
}

fn detect_player(conn: &zbus::blocking::Connection) -> Option<String> {
    let dbus = zbus::blocking::fdo::DBusProxy::new(conn).ok()?;
    let names = dbus.list_names().ok()?;

    let mut first_paused: Option<String> = None;
    for name in names {
        let name = name.as_str();
        if !name.starts_with(MPRIS_PREFIX) {
            continue;
        }
        let Ok(proxy) = player_proxy(conn, name) else {
            continue;
        };
        match proxy.get_property::<String>("PlaybackStatus").as_deref() {
            Ok("Playing") => return Some(name.to_owned()),
            Ok("Paused") if first_paused.is_none() => {
                first_paused = Some(name.to_owned());
            }
            _ => {}
        }
    }
    first_paused
}

fn sleep_interruptible(shutdown: &AtomicBool, dur: Duration) {
    if shutdown.load(Ordering::Relaxed) {
        return;
    }
    std::thread::park_timeout(dur);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn art_cache_decodes_once_per_url() {
        let mut cache = ArtCache::new();
        assert!(cache.resolve(Some("https://example.com/a.jpg")).is_none());
        assert_eq!(cache.url.as_deref(), Some("https://example.com/a.jpg"));
        assert!(cache.resolve(Some("https://example.com/a.jpg")).is_none());
        assert!(cache.resolve(None).is_none());
        assert_eq!(cache.url, None);
    }
}
