use cef::{
    App, Browser, CefString, CommandLine, Frame, ImplApp, ImplCommandLine, ImplFrame,
    ImplRenderProcessHandler, RenderProcessHandler, V8Context, WrapApp, WrapRenderProcessHandler, rc::Rc,
    wrap_app, wrap_render_process_handler,
};

use crate::shim::BRIDGE_INIT;

fn is_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
        || std::env::var("XDG_SESSION_TYPE")
            .map(|v| v == "wayland")
            .unwrap_or(false)
}

fn switch(cmd: &CommandLine, name: &str) {
    let name = CefString::from(name);
    cmd.append_switch(Some(&name));
}

fn switch_val(cmd: &CommandLine, name: &str, value: &str) {
    let name = CefString::from(name);
    let value = CefString::from(value);
    cmd.append_switch_with_value(Some(&name), Some(&value));
}

wrap_render_process_handler! {
    struct ShimRenderProcessHandler {}

    impl RenderProcessHandler {
        fn on_context_created(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _context: Option<&mut V8Context>,
        ) {
            if let Some(frame) = frame {
                let code = CefString::from(BRIDGE_INIT);
                frame.execute_java_script(Some(&code), None, 0);
            }
        }
    }
}

wrap_app! {
    struct KirieApp {}

    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            let Some(cmd) = command_line else { return; };

            switch_val(cmd, "disable-features",
                "IsolateOrigins,HardwareMediaKeyHandling,WebContentsOcclusion,\
                 RendererCodeIntegrityEnabled,site-per-process");
            switch(cmd, "disable-gpu-shader-disk-cache");
            switch(cmd, "disable-site-isolation-trials");
            switch(cmd, "disable-web-security");
            switch_val(cmd, "remote-allow-origins", "*");
            switch_val(cmd, "autoplay-policy", "no-user-gesture-required");
            switch(cmd, "disable-background-timer-throttling");
            switch(cmd, "disable-backgrounding-occluded-windows");
            switch(cmd, "disable-background-media-suspend");
            switch(cmd, "disable-renderer-backgrounding");
            switch(cmd, "disable-breakpad");
            switch(cmd, "disable-field-trial-config");
            switch(cmd, "no-experiments");

            switch(cmd, "allow-file-access-from-files");

            if std::env::var_os("WPE_CEF_NO_IPG").is_none() {
                switch(cmd, "in-process-gpu");
            }

            if is_wayland() {
                let ozone = std::env::var("WPE_CEF_OZONE").unwrap_or_else(|_| "wayland".into());
                switch_val(cmd, "ozone-platform", &ozone);
                switch_val(cmd, "enable-features", "UseOzonePlatform");
                match std::env::var("WPE_CEF_ANGLE").as_deref() {
                    Ok("skip") => {}
                    Ok(v) => switch_val(cmd, "use-angle", v),
                    Err(_) => switch_val(cmd, "use-angle", "gl-egl"),
                }
            }
        }

        fn render_process_handler(&self) -> Option<RenderProcessHandler> {
            Some(ShimRenderProcessHandler::new())
        }
    }
}

#[must_use]
pub fn make_app() -> App {
    KirieApp::new()
}
