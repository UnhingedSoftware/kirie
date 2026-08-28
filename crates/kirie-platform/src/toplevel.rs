use std::collections::HashMap;

use wayland_client::backend::ObjectId;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_handle_v1::State as ToplevelState;

#[derive(Debug, Clone)]
pub(crate) struct PauseConfig {
    pub enabled: bool,
    pub only_active: bool,
    pub ignore_appids: Vec<String>,
}

#[derive(Debug, Default)]
struct Toplevel {
    app_id: String,
    fullscreen: bool,
    activated: bool,
    outputs: Vec<WlOutput>,
}

impl Toplevel {
    fn apply_state(&mut self, raw: &[u8]) {
        self.fullscreen = false;
        self.activated = false;
        for word in raw.as_chunks::<4>().0 {
            match ToplevelState::try_from(u32::from_ne_bytes(*word)) {
                Ok(ToplevelState::Fullscreen) => self.fullscreen = true,
                Ok(ToplevelState::Activated) => self.activated = true,
                _ => {}
            }
        }
    }

    fn blocks(&self, config: &PauseConfig, on_output: bool) -> bool {
        self.fullscreen
            && on_output
            && (!config.only_active || self.activated)
            && !config
                .ignore_appids
                .iter()
                .any(|ignored| ignored.eq_ignore_ascii_case(&self.app_id))
    }
}

pub(crate) struct ToplevelTracker {
    config: PauseConfig,
    toplevels: HashMap<ObjectId, Toplevel>,
    supported: bool,
}

impl ToplevelTracker {
    pub fn new(config: PauseConfig) -> Self {
        Self {
            config,
            toplevels: HashMap::new(),
            supported: false,
        }
    }

    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    pub fn set_supported(&mut self) {
        self.supported = true;
    }

    pub fn finished(&mut self) {
        self.supported = false;
        self.toplevels.clear();
    }

    pub fn track(&mut self, id: ObjectId) {
        self.toplevels.insert(id, Toplevel::default());
    }

    pub fn forget(&mut self, id: &ObjectId) {
        self.toplevels.remove(id);
    }

    pub fn set_app_id(&mut self, id: &ObjectId, app_id: String) {
        if let Some(toplevel) = self.toplevels.get_mut(id) {
            toplevel.app_id = app_id;
        }
    }

    pub fn set_state(&mut self, id: &ObjectId, raw: &[u8]) {
        if let Some(toplevel) = self.toplevels.get_mut(id) {
            toplevel.apply_state(raw);
        }
    }

    pub fn enter_output(&mut self, id: &ObjectId, output: WlOutput) {
        if let Some(toplevel) = self.toplevels.get_mut(id)
            && !toplevel.outputs.contains(&output)
        {
            toplevel.outputs.push(output);
        }
    }

    pub fn leave_output(&mut self, id: &ObjectId, output: &WlOutput) {
        if let Some(toplevel) = self.toplevels.get_mut(id) {
            toplevel.outputs.retain(|o| o != output);
        }
    }

    pub fn blocking_appid(&self, output: &WlOutput) -> Option<&str> {
        if !self.supported || !self.config.enabled {
            return None;
        }
        self.toplevels
            .values()
            .find(|toplevel| toplevel.blocks(&self.config, toplevel.outputs.iter().any(|o| o == output)))
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

    fn state_array(values: &[ToplevelState]) -> Vec<u8> {
        values.iter().flat_map(|v| (*v as u32).to_ne_bytes()).collect()
    }

    #[test]
    fn state_event_replaces_rather_than_accumulates() {
        let mut top = Toplevel::default();
        top.apply_state(&state_array(&[
            ToplevelState::Activated,
            ToplevelState::Fullscreen,
        ]));
        assert!(top.fullscreen && top.activated);

        top.apply_state(&state_array(&[ToplevelState::Activated]));
        assert!(!top.fullscreen);
        assert!(top.activated);

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
        raw.push(0xff);
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
