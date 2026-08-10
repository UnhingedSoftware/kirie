//! The Wallpaper Engine web JS API bridge, shared by every web backend.
//!
//! WE web wallpapers call a small set of `window.wallpaper*` functions to
//! receive audio, user properties and MPRIS media data. In the reference
//! engine the *renderer-process* half of the bridge is injected once per V8
//! context (`SubprocessApp::OnContextCreated`) and the *browser-process* half
//! pushes data into it via `frame->ExecuteJavaScript`
//! (docs/subsystems-misc.md §3.5). kirie reproduces both halves here in a
//! backend-neutral way:
//!
//! - [`BRIDGE_INIT`] is the renderer-side shim, injected as an *initialization
//!   script* so `wallpaperRegisterAudioListener` &c. exist before the page's
//!   own scripts run.
//! - the `*_call` builders produce the one-line JavaScript statements a
//!   backend evaluates each frame (CEF via `ExecuteJavaScript`, the webview via
//!   `WebView::evaluate_script`) to fire those listeners.
//!
//! Both backends import this module so the shim string and the call encodings
//! are defined exactly once (no duplication). Everything here is pure `std`
//! and compiled in the default build (SPEC V9: string building only, never
//! panics on odd audio/property input).

use std::fmt::Write as _;

/// The renderer-side bridge, injected before page scripts.
///
/// Defines the `wallpaperRegister*Listener` registration functions plus the
/// `__wp*` entry points the backend drives, guarded by `window.__wpBridge` so
/// a double injection is a no-op. Mirrors `SubprocessApp::OnContextCreated`
/// (docs/subsystems-misc.md §3.5): audio + the four MPRIS media listeners, and
/// `__wpApplyProps` / `__wpApplyGeneral` which forward to the page's
/// `wallpaperPropertyListener`.
///
/// # Late registration replays the last event
///
/// The shim exists before page scripts run, but a page registers its listeners
/// whenever it likes — real workshop wallpapers do it from a jQuery
/// `$(document).ready` handler, after loading jQuery, lodash, fonts and a few
/// hundred KB of CSS. Media events are sent on **change**, so anything pushed
/// into a registration list that is still empty is not merely late, it is gone
/// for good: the next push only comes when the track does. That is precisely
/// the failure this bridge was built to end — a media wallpaper stuck on
/// "Loading…" while the engine dutifully sends data nobody is listening for.
///
/// So each channel remembers its most recent payload and hands it straight to
/// any listener that registers afterwards. Registration order and page load
/// time stop mattering, and a page that registers early is unaffected (it just
/// gets the event twice: once on registration if something already arrived,
/// then normally). Only the *latest* value per channel is kept, so this is a
/// handful of references, not a log.
pub const BRIDGE_INIT: &str = r#"(function(){
  if (window.__wpBridge) { return; }
  window.__wpBridge = true;
  var lists = {};
  var latest = {};
  function register(name, key) {
    lists[key] = [];
    window[name] = function (cb) {
      if (typeof cb !== 'function') { return; }
      lists[key].push(cb);
      if (Object.prototype.hasOwnProperty.call(latest, key)) {
        try { cb(latest[key]); } catch (e) { /* see fire() */ }
      }
    };
  }
  function fire(key, data) {
    latest[key] = data;
    var cbs = lists[key] || [];
    for (var i = 0; i < cbs.length; i++) {
      try { cbs[i](data); } catch (e) { /* a broken page listener must not break the bridge */ }
    }
  }
  register('wallpaperRegisterAudioListener', 'audio');
  register('wallpaperRegisterMediaPropertiesListener', 'mprops');
  register('wallpaperRegisterMediaPlaybackListener', 'mplayback');
  register('wallpaperRegisterMediaThumbnailListener', 'mthumb');
  register('wallpaperRegisterMediaTimelineListener', 'mtimeline');
  register('wallpaperRegisterMediaStatusListener', 'mstatus');
  window.__wpAudio = function (d) { fire('audio', d); };
  window.__wpMediaProps = function (d) { fire('mprops', d); };
  window.__wpMediaPlayback = function (d) { fire('mplayback', d); };
  window.__wpMediaThumb = function (d) { fire('mthumb', d); };
  window.__wpMediaTimeline = function (d) { fire('mtimeline', d); };
  window.__wpMediaStatus = function (d) { fire('mstatus', d); };
  window.wallpaperRequestRandomFileForProperty = function (name, cb) {
    if (typeof cb === 'function') { try { cb(name, ''); } catch (e) {} }
  };
  // `wallpaperPropertyListener` is assigned, not registered, so it needs the
  // same late-arrival care as the listeners above but through a property
  // setter: the engine sends the initial property batch as soon as the
  // document commits, which is before the page's own scripts have assigned
  // the listener. Without replay the batch lands on nothing and the page runs
  // on its JS defaults forever — ION, for one, defaults its particle count to
  // 0, so the wallpaper visibly loses a whole feature.
  var propListener = undefined;
  var lastUser;
  var lastGeneral;
  Object.defineProperty(window, 'wallpaperPropertyListener', {
    configurable: true,
    get: function () { return propListener; },
    set: function (l) {
      propListener = l;
      if (!l) { return; }
      if (lastUser !== undefined && typeof l.applyUserProperties === 'function') {
        try { l.applyUserProperties(lastUser); } catch (e) { /* page's problem, not the bridge's */ }
      }
      if (lastGeneral !== undefined && typeof l.applyGeneralProperties === 'function') {
        try { l.applyGeneralProperties(lastGeneral); } catch (e) {}
      }
    }
  });
  window.__wpApplyProps = function (p) {
    lastUser = p;
    var l = propListener;
    if (l && typeof l.applyUserProperties === 'function') {
      try { l.applyUserProperties(p); } catch (e) {}
    }
  };
  window.__wpApplyGeneral = function (p) {
    lastGeneral = p;
    var l = propListener;
    if (l && typeof l.applyGeneralProperties === 'function') {
      try { l.applyGeneralProperties(p); } catch (e) {}
    }
  };
})();"#;

/// Build the per-frame `__wpAudio([...])` call from FFT magnitudes.
///
/// WE delivers **128** floats — 64 bands duplicated as identical left+right
/// channels, each formatted `"%.4f"` (docs/subsystems-misc.md §1.3, §3.5). A
/// 64-length `bands` slice is mirrored to 128; any other length is used
/// verbatim (padded/truncated to at least keep valid JS), so malformed audio
/// input can never panic (SPEC V9).
#[must_use]
pub fn audio_call(bands: &[f32]) -> String {
    // Reproduce the reference layout: 64 bands, twice.
    let mirror = bands.len() == 64;
    let count = if mirror { 128 } else { bands.len() };
    let mut js = String::with_capacity(count * 8 + 16);
    js.push_str("window.__wpAudio([");
    for i in 0..count {
        let v = if mirror { bands[i % 64] } else { bands[i] };
        if i != 0 {
            js.push(',');
        }
        // `{:.4}` matches the reference "%.4f"; guard non-finite to 0.
        let v = if v.is_finite() { v } else { 0.0 };
        let _ = write!(js, "{v:.4}");
    }
    js.push_str("]);");
    js
}

/// Build the one-shot `__wpApplyProps({...})` call.
///
/// `json` must already be a serialized JSON object of the shape
/// `{name: {value: ...}}` (the caller performs the typed color/bool/slider
/// serialization described in docs/subsystems-misc.md §3.5). It is spliced in
/// verbatim.
#[must_use]
pub fn apply_user_properties_call(json: &str) -> String {
    format!("window.__wpApplyProps({json});")
}

/// Build the `__wpApplyGeneral({...})` call (engine/general properties).
#[must_use]
pub fn apply_general_properties_call(json: &str) -> String {
    format!("window.__wpApplyGeneral({json});")
}

/// Build the `__wpMediaProps({title, artist, album})` call from serialized JSON.
#[must_use]
pub fn media_properties_call(json: &str) -> String {
    format!("window.__wpMediaProps({json});")
}

/// Build the `__wpMediaPlayback({state})` call from serialized JSON.
#[must_use]
pub fn media_playback_call(json: &str) -> String {
    format!("window.__wpMediaPlayback({json});")
}

/// Build the `__wpMediaTimeline({position, duration})` call from serialized JSON.
#[must_use]
pub fn media_timeline_call(json: &str) -> String {
    format!("window.__wpMediaTimeline({json});")
}

/// Build the `__wpMediaThumb({thumbnail, primaryColor, ...})` call from JSON.
#[must_use]
pub fn media_thumbnail_call(json: &str) -> String {
    format!("window.__wpMediaThumb({json});")
}

/// Build the `__wpMediaStatus({enabled})` call from serialized JSON.
///
/// The status listener is the one the page uses to decide whether to show its
/// media UI at all: [`BRIDGE_INIT`] has always registered it, but nothing built
/// its call until the media feed landed, so a page that gates on it stayed
/// blank even with everything else wired.
#[must_use]
pub fn media_status_call(json: &str) -> String {
    format!("window.__wpMediaStatus({json});")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_mirrors_64_bands_to_128() {
        let bands: Vec<f32> = (0..64).map(|i| i as f32 / 64.0).collect();
        let js = audio_call(&bands);
        // 128 comma-separated values → 127 commas between them.
        assert_eq!(js.matches(',').count(), 127);
        assert!(js.starts_with("window.__wpAudio(["));
        assert!(js.ends_with("]);"));
    }

    #[test]
    fn audio_handles_non_finite_without_panic() {
        let bands = [f32::NAN, f32::INFINITY, -1.0, 2.0];
        let js = audio_call(&bands);
        assert!(js.contains("0.0000"));
        // Non-64 length is used verbatim: 4 values, 3 commas.
        assert_eq!(js.matches(',').count(), 3);
    }

    #[test]
    fn property_calls_splice_json() {
        assert_eq!(
            apply_user_properties_call("{\"a\":{\"value\":1}}"),
            "window.__wpApplyProps({\"a\":{\"value\":1}});"
        );
    }
}
