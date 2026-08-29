use std::fmt::Write as _;

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
  // A page may assign the listener from inside its render, so each batch is
  // replayed at most once: replaying on every assignment loops apply -> render
  // -> assign -> apply until the framework tears the tree down.
  var propListener = undefined;
  var lastUser;
  var lastGeneral;
  var userDelivered = false;
  var generalDelivered = false;
  Object.defineProperty(window, 'wallpaperPropertyListener', {
    configurable: true,
    get: function () { return propListener; },
    set: function (l) {
      propListener = l;
      if (!l) { return; }
      if (lastUser !== undefined && !userDelivered && typeof l.applyUserProperties === 'function') {
        userDelivered = true;
        try { l.applyUserProperties(lastUser); } catch (e) { /* page's problem, not the bridge's */ }
      }
      if (lastGeneral !== undefined && !generalDelivered && typeof l.applyGeneralProperties === 'function') {
        generalDelivered = true;
        try { l.applyGeneralProperties(lastGeneral); } catch (e) {}
      }
    }
  });
  window.__wpApplyProps = function (p) {
    lastUser = p;
    var l = propListener;
    var can = l && typeof l.applyUserProperties === 'function';
    userDelivered = !!can;
    if (can) {
      try { l.applyUserProperties(p); } catch (e) {}
    }
  };
  window.__wpApplyGeneral = function (p) {
    lastGeneral = p;
    var l = propListener;
    var can = l && typeof l.applyGeneralProperties === 'function';
    generalDelivered = !!can;
    if (can) {
      try { l.applyGeneralProperties(p); } catch (e) {}
    }
  };
})();"#;

#[must_use]
pub fn audio_call(bands: &[f32]) -> String {
    let mirror = bands.len() == 64;
    let count = if mirror { 128 } else { bands.len() };
    let mut js = String::with_capacity(count * 8 + 16);
    js.push_str("window.__wpAudio([");
    for i in 0..count {
        let v = if mirror { bands[i % 64] } else { bands[i] };
        if i != 0 {
            js.push(',');
        }
        let v = if v.is_finite() { v } else { 0.0 };
        let _ = write!(js, "{v:.4}");
    }
    js.push_str("]);");
    js
}

#[must_use]
pub fn apply_user_properties_call(json: &str) -> String {
    format!("window.__wpApplyProps({json});")
}

#[must_use]
pub fn apply_general_properties_call(json: &str) -> String {
    format!("window.__wpApplyGeneral({json});")
}

#[must_use]
pub fn media_properties_call(json: &str) -> String {
    format!("window.__wpMediaProps({json});")
}

#[must_use]
pub fn media_playback_call(json: &str) -> String {
    format!("window.__wpMediaPlayback({json});")
}

#[must_use]
pub fn media_timeline_call(json: &str) -> String {
    format!("window.__wpMediaTimeline({json});")
}

#[must_use]
pub fn media_thumbnail_call(json: &str) -> String {
    format!("window.__wpMediaThumb({json});")
}

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
        assert_eq!(js.matches(',').count(), 127);
        assert!(js.starts_with("window.__wpAudio(["));
        assert!(js.ends_with("]);"));
    }

    #[test]
    fn audio_handles_non_finite_without_panic() {
        let bands = [f32::NAN, f32::INFINITY, -1.0, 2.0];
        let js = audio_call(&bands);
        assert!(js.contains("0.0000"));
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
