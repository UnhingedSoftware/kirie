//! Foreign-toplevel tracking for the fullscreen pause
//! (`--no-fullscreen-pause` / `--fullscreen-pause-only-active` /
//! `--fullscreen-pause-ignore-appid`, docs/compat-cli.md §2).
//!
//! The reference engine stops rendering a wallpaper entirely while a fullscreen
//! application covers that screen, so a game gets the whole GPU instead of
//! sharing it with a scene that is not even visible. A wlr-layer-shell client
//! has no protocol of its own for noticing that: nothing in `wl_surface` or
//! `zwlr_layer_surface_v1` reports being covered, and the swapchain's
//! `Occluded` status is a driver-level hint that never arrives here. What a
//! covered wallpaper *does* get under wlroots is silence — the compositor stops
//! compositing it, stops delivering frame callbacks and stops releasing
//! buffers, so the next `get_current_texture` simply blocks inside the WSI
//! (observed directly: the render thread parks in `ppoll` instead of the event
//! loop's `epoll_wait`). That is not a pause, it is a stall: the calloop event
//! loop stops turning, so IPC, playlist rotation and output hotplug all freeze
//! with it, and on compositors that keep the wallpaper composited under a
//! fullscreen window it does not even save the GPU.
//!
//! `zwlr_foreign_toplevel_manager_v1` — the protocol taskbars use to enumerate
//! open windows — is the one client-visible source that reports exactly the
//! three things the flags need: a toplevel's `app_id`, its
//! `fullscreen`/`activated` state, and which outputs it is on. Deciding from
//! that lets `draw` bow out *before* touching the swapchain, which keeps the
//! event loop live while doing no work at all.
//!
//! This module is pure bookkeeping: it owns the mirrored toplevel list and the
//! pause rule. Binding the global and the `Dispatch` plumbing live in
//! `src/platform.rs`, where the wayland state lives.
//!
//! Compositors that do not implement the protocol (notably GNOME/Mutter, which
//! deliberately ships no foreign-toplevel interface) simply never mark the
//! tracker supported, and [`ToplevelTracker::blocking_appid`] then always
//! answers `None` — the wallpaper keeps rendering exactly as before. That is an
//! accepted limitation, not a failure mode, so it is logged once at debug level
//! and never surfaced as an error.

use std::collections::HashMap;

use wayland_client::backend::ObjectId;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_handle_v1::State as ToplevelState;

/// How the fullscreen pause should behave, mirrored from
/// [`crate::PresentOptions`] at connect time.
#[derive(Debug, Clone)]
pub(crate) struct PauseConfig {
    /// Pause at all. `false` is `--no-fullscreen-pause`.
    pub enabled: bool,
    /// Only pause when the fullscreen toplevel is *also* focused
    /// (`--fullscreen-pause-only-active`). Without it a fullscreen window that
    /// lost focus still pauses, matching the reference default: an
    /// alt-tabbed-away game is still covering the wallpaper.
    pub only_active: bool,
    /// `app_id`s that never cause a pause (`--fullscreen-pause-ignore-appid`),
    /// compared case-insensitively.
    pub ignore_appids: Vec<String>,
}

/// One mirrored `zwlr_foreign_toplevel_handle_v1`.
///
/// Only the fields the pause rule reads are kept; `title`/`parent` are ignored.
#[derive(Debug, Default)]
struct Toplevel {
    /// Latest `app_id` event value; empty until the compositor sends one.
    app_id: String,
    /// `fullscreen` was in the latest `state` array (protocol version ≥ 2).
    fullscreen: bool,
    /// `activated` was in the latest `state` array — i.e. this is the focused
    /// window.
    activated: bool,
    /// Outputs this toplevel is currently visible on, from
    /// `output_enter`/`output_leave`. A toplevel may span several.
    outputs: Vec<WlOutput>,
}

impl Toplevel {
    /// Decode a `state` event array into the two flags this cares about.
    ///
    /// The array carries the toplevel's *complete* current state list as packed
    /// host-endian `u32` enum values, not a delta, so both flags are recomputed
    /// from zero — that is what clears `fullscreen` when a window is restored,
    /// and dropping it would leave the wallpaper paused for the rest of the
    /// session.
    fn apply_state(&mut self, raw: &[u8]) {
        self.fullscreen = false;
        self.activated = false;
        // `as_chunks` drops a trailing partial value rather than panicking; the
        // wire format guarantees a multiple of 4, but this is compositor input
        // (SPEC V9: no unchecked arithmetic or indexing on it).
        for word in raw.as_chunks::<4>().0 {
            match ToplevelState::try_from(u32::from_ne_bytes(*word)) {
                // `fullscreen` only exists since protocol version 2; a v1
                // compositor simply never sets it and never pauses.
                Ok(ToplevelState::Fullscreen) => self.fullscreen = true,
                Ok(ToplevelState::Activated) => self.activated = true,
                _ => {}
            }
        }
    }

    /// Whether this toplevel should pause a wallpaper, given that it is
    /// (`on_output`) or is not on the output in question.
    ///
    /// Split out from the output lookup so the rule itself is plain data and
    /// can be tested without a compositor.
    fn blocks(&self, config: &PauseConfig, on_output: bool) -> bool {
        self.fullscreen
            && on_output
            && (!config.only_active || self.activated)
            // `app_id`s are reverse-DNS/ASCII identifiers in practice
            // (`steam_app_12345`, `org.kde.konsole`), so ASCII-insensitive
            // matching is both correct and allocation-free.
            && !config
                .ignore_appids
                .iter()
                .any(|ignored| ignored.eq_ignore_ascii_case(&self.app_id))
    }
}

/// Mirror of every open toplevel plus the pause rule over it.
///
/// Kept deliberately small: a handful of entries, rebuilt from scratch by the
/// compositor whenever we (re)connect, so a linear scan per decision is far
/// cheaper than any index would be.
pub(crate) struct ToplevelTracker {
    config: PauseConfig,
    /// Keyed by the handle's protocol object id — the only identity a
    /// `zwlr_foreign_toplevel_handle_v1` has.
    toplevels: HashMap<ObjectId, Toplevel>,
    /// The compositor advertised `zwlr_foreign_toplevel_manager_v1` and we
    /// bound it. `false` (GNOME, or a `finished` manager) forces
    /// [`Self::blocking_appid`] to `None`, so the wallpaper never pauses rather
    /// than pausing on stale information.
    supported: bool,
}

impl ToplevelTracker {
    /// A tracker that is not yet backed by a bound manager global.
    pub fn new(config: PauseConfig) -> Self {
        Self {
            config,
            toplevels: HashMap::new(),
            supported: false,
        }
    }

    /// Whether the pause is switched on at all (`--no-fullscreen-pause`
    /// clears it). Checked before binding the global so an opted-out run does
    /// not carry the protocol traffic.
    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    /// Record that the manager global was bound; only then may this tracker
    /// pause anything.
    pub fn set_supported(&mut self) {
        self.supported = true;
    }

    /// The manager sent `finished`: the compositor destroyed it and will send
    /// nothing more. Every mirrored entry is now stale, so drop the lot and
    /// stop pausing — a wallpaper that stayed paused on information that can
    /// no longer be updated would be frozen forever.
    pub fn finished(&mut self) {
        self.supported = false;
        self.toplevels.clear();
    }

    /// Start mirroring a newly announced toplevel. Its `app_id`/`state`/output
    /// events follow immediately and land through the setters below.
    pub fn track(&mut self, id: ObjectId) {
        self.toplevels.insert(id, Toplevel::default());
    }

    /// Stop mirroring a closed toplevel. This is the event that ends a pause
    /// in the common case (the game exits), so it must never be dropped.
    pub fn forget(&mut self, id: &ObjectId) {
        self.toplevels.remove(id);
    }

    /// Apply an `app_id` event.
    pub fn set_app_id(&mut self, id: &ObjectId, app_id: String) {
        if let Some(toplevel) = self.toplevels.get_mut(id) {
            toplevel.app_id = app_id;
        }
    }

    /// Apply a `state` event (see [`Toplevel::apply_state`]).
    pub fn set_state(&mut self, id: &ObjectId, raw: &[u8]) {
        if let Some(toplevel) = self.toplevels.get_mut(id) {
            toplevel.apply_state(raw);
        }
    }

    /// Apply an `output_enter` event.
    pub fn enter_output(&mut self, id: &ObjectId, output: WlOutput) {
        if let Some(toplevel) = self.toplevels.get_mut(id)
            && !toplevel.outputs.contains(&output)
        {
            toplevel.outputs.push(output);
        }
    }

    /// Apply an `output_leave` event.
    pub fn leave_output(&mut self, id: &ObjectId, output: &WlOutput) {
        if let Some(toplevel) = self.toplevels.get_mut(id) {
            toplevel.outputs.retain(|o| o != output);
        }
    }

    /// The `app_id` of a toplevel that should pause the wallpaper on `output`,
    /// or `None` when it should keep rendering.
    ///
    /// The rule, in full: pausing is enabled, the manager global is bound, and
    /// some toplevel is **fullscreen**, **on this output**, **not in the ignore
    /// list**, and — with `--fullscreen-pause-only-active` — **activated**.
    ///
    /// Output membership is required rather than assumed: on a multi-head
    /// desktop a game fullscreened on one monitor must not blank the wallpaper
    /// on the others. The flip side is that a compositor which implements the
    /// protocol but never emits `output_enter` can never pause; that is the
    /// same accepted limitation as not implementing the protocol at all, and it
    /// fails safe (keeps rendering).
    pub fn blocking_appid(&self, output: &WlOutput) -> Option<&str> {
        if !self.supported || !self.config.enabled {
            return None;
        }
        self.toplevels
            .values()
            .find(|toplevel| {
                toplevel.blocks(&self.config, toplevel.outputs.iter().any(|o| o == output))
            })
            .map(|toplevel| toplevel.app_id.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(only_active: bool, ignore: &[&str]) -> PauseConfig {
        PauseConfig {
            enabled: true,
            only_active,
            ignore_appids: ignore.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    /// Packed `state` array as the compositor sends it (host-endian `u32`s).
    fn state_array(values: &[ToplevelState]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|v| (*v as u32).to_ne_bytes())
            .collect()
    }

    #[test]
    fn state_event_replaces_rather_than_accumulates() {
        let mut top = Toplevel::default();
        top.apply_state(&state_array(&[
            ToplevelState::Activated,
            ToplevelState::Fullscreen,
        ]));
        assert!(top.fullscreen && top.activated);

        // Un-fullscreening sends the remaining states only; the flag must clear,
        // or the wallpaper would stay paused forever.
        top.apply_state(&state_array(&[ToplevelState::Activated]));
        assert!(!top.fullscreen);
        assert!(top.activated);

        // Maximized/minimized are irrelevant to the rule and must not set either.
        top.apply_state(&state_array(&[
            ToplevelState::Maximized,
            ToplevelState::Minimized,
        ]));
        assert!(!top.fullscreen && !top.activated);
    }

    #[test]
    fn truncated_state_array_is_ignored_not_fatal() {
        let mut top = Toplevel::default();
        let mut raw = state_array(&[ToplevelState::Fullscreen]);
        raw.push(0xff); // stray trailing byte
        top.apply_state(&raw);
        assert!(top.fullscreen);
    }

    #[test]
    fn only_fullscreen_on_this_output_blocks() {
        let mut top = Toplevel {
            app_id: "steam_app_12345".to_owned(),
            ..Toplevel::default()
        };
        let cfg = config(false, &[]);
        assert!(!top.blocks(&cfg, true), "not fullscreen");

        top.fullscreen = true;
        assert!(top.blocks(&cfg, true));
        assert!(!top.blocks(&cfg, false), "fullscreen on a different monitor");
    }

    #[test]
    fn only_active_requires_the_activated_state() {
        let top = Toplevel {
            app_id: "steam_app_12345".to_owned(),
            fullscreen: true,
            activated: false,
            outputs: Vec::new(),
        };
        // Default: an alt-tabbed-away fullscreen window still covers the
        // wallpaper, so it still pauses.
        assert!(top.blocks(&config(false, &[]), true));
        assert!(!top.blocks(&config(true, &[]), true));
    }

    #[test]
    fn ignore_list_matches_case_insensitively() {
        let top = Toplevel {
            app_id: "org.KDE.Konsole".to_owned(),
            fullscreen: true,
            activated: true,
            outputs: Vec::new(),
        };
        assert!(!top.blocks(&config(false, &["org.kde.konsole"]), true));
        assert!(top.blocks(&config(false, &["mpv"]), true));
    }
}
