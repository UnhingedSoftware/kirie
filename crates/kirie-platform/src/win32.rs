//! The Win32 calls the Windows backend needs, each wrapped so that the backend
//! itself reads as ordinary Rust.
//!
//! Everything unsafe in the backend lives here. The rule for this file is that
//! a wrapper is safe to call for any argument a caller can construct: handles
//! are checked for null before use, buffers are sized before they are passed,
//! and nothing hands out a pointer that outlives the call.

use std::ffi::c_void;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MONITORINFOEXW, MonitorFromWindow,
};
use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowExW, FindWindowW, GW_CHILD,
    GW_HWNDNEXT, GWL_EXSTYLE, GetClassNameW, GetCursorPos, GetForegroundWindow, GetSystemMetrics, GetWindow,
    GetWindowLongW, GetWindowRect, GetWindowThreadProcessId, HWND_BOTTOM, HWND_TOP, IsWindow, LWA_ALPHA, MSG,
    PM_REMOVE, PeekMessageW, RegisterClassW, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN, SMTO_NORMAL, SW_SHOWNA, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SendMessageTimeoutW, SetLayeredWindowAttributes, SetParent, SetWindowPos, ShowWindow, WM_ERASEBKGND,
    WNDCLASSW, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
};

use crate::desktop_tree::{self, Layout, Seen, Snapshot};

/// A window on the desktop, kept as a raw handle because that is what both
/// Win32 and wgpu want to be handed.
pub(crate) type Handle = HWND;

/// One monitor as Windows describes it.
pub(crate) struct Monitor {
    /// `DISPLAY1`, `DISPLAY2` ... which is how Windows numbers screens in its
    /// own display settings, so it is the name a user can match up.
    pub(crate) name: String,
    /// Physical pixels, in virtual-screen coordinates, which can be negative
    /// for a monitor placed left of or above the primary one.
    pub(crate) rect: Rect,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Rect {
    pub(crate) left: i32,
    pub(crate) top: i32,
    pub(crate) right: i32,
    pub(crate) bottom: i32,
}

impl Rect {
    pub(crate) fn width(self) -> u32 {
        self.right.saturating_sub(self.left).max(1) as u32
    }

    pub(crate) fn height(self) -> u32 {
        self.bottom.saturating_sub(self.top).max(1) as u32
    }

    pub(crate) const fn contains(self, point: (i32, i32)) -> bool {
        point.0 >= self.left && point.0 < self.right && point.1 >= self.top && point.1 < self.bottom
    }

    const fn from_win32(rect: RECT) -> Self {
        Self {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        }
    }
}

/// Ask Windows to report real pixels rather than the stretched coordinates it
/// hands a program that has not said it understands scaling.
///
/// Without this, every monitor rect on a 150% display comes back two thirds of
/// its real size and the wallpaper is drawn blurry and upscaled. It has to
/// happen before the first window exists, and it is a no-op on the second call.
#[allow(unsafe_code)]
pub(crate) fn announce_dpi_awareness() {
    // SAFETY: a process-wide setting taking a constant the API defines; it
    // fails harmlessly if something already set an awareness mode.
    let set = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    if set == 0 {
        tracing::debug!("per-monitor dpi awareness was already set, or is unavailable");
    }
}

/// Make this thread a single-threaded COM apartment.
///
/// A web wallpaper's WebView2 insists on one, on the thread that owns its
/// window, and delivers every callback through that thread's message queue.
/// The render loop is that thread and already pumps it, so all it needs is to
/// be declared an apartment before anything else claims it as multi-threaded.
/// Nothing else the backend does minds which kind it is.
#[allow(unsafe_code)]
pub(crate) fn enter_apartment() {
    // SAFETY: the reserved argument is null as documented, and the call only
    // sets this thread's COM mode. It is never undone, which is fine for a
    // thread that lives as long as the process.
    let entered = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
    if entered < 0 {
        tracing::warn!(
            hresult = entered,
            "COM was already set up differently here; web wallpapers may not open"
        );
    }
}

/// Every monitor currently attached, in the order Windows enumerates them.
#[allow(unsafe_code)]
pub(crate) fn monitors() -> Vec<Monitor> {
    let mut found: Vec<Monitor> = Vec::new();
    // SAFETY: the callback matches the signature EnumDisplayMonitors expects,
    // and the lparam is the address of `found`, which outlives the call because
    // the call returns before this function does.
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(collect_monitor),
            std::ptr::from_mut(&mut found) as LPARAM,
        );
    }
    found
}

#[allow(unsafe_code)]
unsafe extern "system" fn collect_monitor(monitor: HMONITOR, _dc: HDC, _clip: *mut RECT, out: LPARAM) -> i32 {
    // SAFETY: MONITORINFOEXW is a plain C struct with no invalid bit patterns,
    // and cbSize is set to the size the API checks for immediately below.
    let mut info: MONITORINFOEXW = unsafe { std::mem::zeroed() };
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;

    // SAFETY: `info` is sized as the API requires and lives for the call.
    let ok = unsafe { GetMonitorInfoW(monitor, std::ptr::from_mut(&mut info).cast::<MONITORINFO>()) };
    if ok == 0 {
        return 1;
    }

    // SAFETY: the lparam is the `&mut Vec<Monitor>` monitors() passed in, and
    // EnumDisplayMonitors calls this only from inside that call.
    let found = unsafe { &mut *(out as *mut Vec<Monitor>) };
    found.push(Monitor {
        name: display_name(&info.szDevice, found.len()),
        rect: Rect::from_win32(info.monitorInfo.rcMonitor),
    });
    1
}

/// `\\.\DISPLAY2` is how Windows names a monitor internally; `DISPLAY2` is the
/// same name with the device prefix taken off, which is short enough to type at
/// `--screen-root` and still matches the numbering in display settings.
fn display_name(device: &[u16; 32], index: usize) -> String {
    let end = device.iter().position(|c| *c == 0).unwrap_or(device.len());
    let full = String::from_utf16_lossy(device.get(..end).unwrap_or_default());
    match full.rsplit('\\').next().filter(|tail| !tail.is_empty()) {
        Some(tail) => tail.to_owned(),
        None => format!("Screen-{index}"),
    }
}

/// The whole desktop as one rectangle, which is the coordinate space the
/// wallpaper host's client area uses.
#[allow(unsafe_code)]
pub(crate) fn virtual_screen() -> Rect {
    // SAFETY: a plain query taking constants the API defines.
    let (x, y, w, h) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    Rect {
        left: x,
        top: y,
        right: x.saturating_add(w),
        bottom: y.saturating_add(h),
    }
}

/// Where on the desktop the wallpaper windows go, with the windows found there.
pub(crate) type Desktop = Layout<Handle>;

/// Find where the wallpaper goes, the way Wallpaper Engine and Lively do.
///
/// The desktop is Explorer's `Progman`, and there is only room for a wallpaper
/// behind its icons once it has been asked to make some. The way to ask is an
/// undocumented message, `0x052C`, which has been the way since Windows 7.
/// What it makes changed in Windows 11 24H2 (`desktop_tree` has both shapes),
/// so the message is only sent when what is there is not enough, and then
/// Explorer is given a moment: it can make the new window lazily.
///
/// A classic desktop that will not split is the one case left over. The
/// wallpaper then goes on Progman in front of the icons, which is visible and
/// wrong rather than invisible, and says so in the log.
pub(crate) fn find_desktop() -> Option<Desktop> {
    let mut seen = snapshot()?;
    let mut layout = desktop_tree::choose(&seen);
    if layout.wants_split() {
        split(seen.progman);
        for _ in 0..SPLIT_POLLS {
            std::thread::sleep(SPLIT_POLL);
            let Some(again) = snapshot() else {
                break;
            };
            seen = again;
            layout = desktop_tree::choose(&seen);
            if !layout.wants_split() {
                break;
            }
        }
    }

    let classes = |windows: &[Seen<Handle>]| {
        windows
            .iter()
            .map(|window| {
                format!(
                    "{}{}",
                    window.class,
                    if window.has_icons { "(icons)" } else { "" }
                )
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    tracing::info!(
        raised = seen.raised_style,
        progman_children = %classes(&seen.progman_children),
        top_level_workers = %classes(&seen.top_workers),
        ?layout,
        "found the desktop"
    );
    match layout {
        Layout::Unsplit { .. } => tracing::warn!(
            "Explorer did not make a window behind the desktop icons; drawing in front of them instead"
        ),
        Layout::UnderIcons { layer: None, .. } => tracing::warn!(
            "Explorer has not made its wallpaper layer, so the icons may still be drawing the wallpaper over ours"
        ),
        Layout::UnderIcons { .. } | Layout::Behind { .. } => {}
    }
    Some(layout)
}

/// The message that makes Progman split the desktop. Undocumented; `0xD, 1`
/// is what Lively, Seelen and FeatherWall send, and unlike `0, 0` it does not
/// depend on "Animate controls and elements inside windows" being on.
const SPLIT_DESKTOP: u32 = 0x052C;

const SPLIT_TIMEOUT_MS: u32 = 1_000;

/// How often, and how many times, to look again after asking for the split.
const SPLIT_POLL: std::time::Duration = std::time::Duration::from_millis(100);
const SPLIT_POLLS: u32 = 20;

#[allow(unsafe_code)]
fn split(progman: Handle) {
    // SAFETY: a message send with a timeout, to a window we just found, whose
    // result we do not read.
    unsafe {
        SendMessageTimeoutW(
            progman,
            SPLIT_DESKTOP,
            0xD,
            0x1,
            SMTO_NORMAL,
            SPLIT_TIMEOUT_MS,
            std::ptr::null_mut(),
        );
    }
}

/// Explorer's desktop windows as they are now.
#[allow(unsafe_code)]
fn snapshot() -> Option<Snapshot<Handle>> {
    // SAFETY: FindWindowW takes a null-terminated class name and no title.
    let progman = unsafe { FindWindowW(wide("Progman").as_ptr(), std::ptr::null()) };
    if progman.is_null() {
        tracing::warn!("no Progman window; is Explorer running?");
        return None;
    }
    Some(snapshot_of(progman))
}

#[allow(unsafe_code)]
fn snapshot_of(progman: Handle) -> Snapshot<Handle> {
    // The top-level `WorkerW` windows in z-order: each search starts after the
    // one before, and a null parent keeps it to top-level windows.
    let class = wide(desktop_tree::WORKER);
    let mut top_workers = Vec::new();
    let mut after: Handle = std::ptr::null_mut();
    while top_workers.len() < MAX_WINDOWS {
        // SAFETY: `after` is null or a window the previous search returned, and
        // `class` is a null-terminated string that outlives the call.
        let next = unsafe { FindWindowExW(std::ptr::null_mut(), after, class.as_ptr(), std::ptr::null()) };
        if next.is_null() {
            break;
        }
        top_workers.push(seen(next));
        after = next;
    }

    Snapshot {
        progman,
        raised_style: ex_style(progman) & WS_EX_NOREDIRECTIONBITMAP != 0,
        progman_children: children(progman).into_iter().map(seen).collect(),
        top_workers,
    }
}

/// No desktop has this many, so a search that gets here is going round in
/// circles on windows that are being made and destroyed under it.
const MAX_WINDOWS: usize = 256;

fn seen(window: Handle) -> Seen<Handle> {
    Seen {
        window,
        class: class_of(window),
        has_icons: has_icons(window),
    }
}

/// A window's direct children, topmost first.
#[allow(unsafe_code)]
fn children(parent: Handle) -> Vec<Handle> {
    let mut found = Vec::new();
    // SAFETY: GetWindow is defined for any handle, and answers null for a
    // window that has gone.
    let mut next = unsafe { GetWindow(parent, GW_CHILD) };
    while !next.is_null() && found.len() < MAX_WINDOWS {
        found.push(next);
        // SAFETY: as above.
        next = unsafe { GetWindow(next, GW_HWNDNEXT) };
    }
    found
}

#[allow(unsafe_code)]
fn class_of(window: Handle) -> String {
    let mut name = [0u16; 256];
    // SAFETY: the buffer's real length is passed, and the call writes at most
    // that many characters including the terminator.
    let length = unsafe { GetClassNameW(window, name.as_mut_ptr(), name.len() as i32) };
    let length = usize::try_from(length).unwrap_or(0).min(name.len());
    String::from_utf16_lossy(name.get(..length).unwrap_or_default())
}

#[allow(unsafe_code)]
fn has_icons(window: Handle) -> bool {
    let class = wide(desktop_tree::ICONS);
    // SAFETY: a search of `window`'s direct children for a null-terminated
    // class name that outlives the call; a dead `window` finds nothing.
    let icons = unsafe { FindWindowExW(window, std::ptr::null_mut(), class.as_ptr(), std::ptr::null()) };
    !icons.is_null()
}

#[allow(unsafe_code)]
fn ex_style(window: Handle) -> u32 {
    // SAFETY: reading a window's extended style, which is defined for any
    // handle and answers 0 for a dead one.
    (unsafe { GetWindowLongW(window, GWL_EXSTYLE) }) as u32
}

/// Whether a window handle still names a live window.
#[allow(unsafe_code)]
pub(crate) fn alive(window: Handle) -> bool {
    if window.is_null() {
        return false;
    }
    // SAFETY: IsWindow is defined for any handle value, live or stale.
    unsafe { IsWindow(window) != 0 }
}

/// A screen's wallpaper window, and the layered window it sits in on a raised
/// desktop.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Made {
    pub(crate) window: Handle,
    pub(crate) holder: Option<Handle>,
}

impl Made {
    /// The window that is a child of the desktop, and so the one to move.
    pub(crate) fn outer(self) -> Handle {
        self.holder.unwrap_or(self.window)
    }

    pub(crate) fn alive(self) -> bool {
        alive(self.window) && self.holder.is_none_or(alive)
    }
}

/// Make a borderless child window covering `rect`, inside the desktop.
///
/// `rect` is in virtual-screen coordinates; the desktop's client area starts at
/// the virtual screen's own origin, so the offset between the two is what turns
/// one into the other.
///
/// On a raised desktop the window goes inside a layered window of its own,
/// which is what Microsoft asks for there: Progman has no redirection surface,
/// so a plain child of it has nowhere for a copied (blt) present to land, and
/// only presents that bypass it would show. The layered window gives every
/// present somewhere to go, and the wallpaper window inside it stays an
/// ordinary child for wgpu or a web view to draw into. If Windows will not make
/// one, the wallpaper window goes straight into Progman, which works for the
/// presents that do not need it. `KIRIE_NO_LAYERED_HOST` asks for that.
#[allow(unsafe_code)]
pub(crate) fn desktop_window(desktop: &Desktop, rect: Rect, take_clicks: bool) -> Option<Made> {
    register_class()?;

    let origin = virtual_screen();
    let at = (
        rect.left.saturating_sub(origin.left),
        rect.top.saturating_sub(origin.top),
    );
    let style_ex = if take_clicks {
        WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW
    } else {
        // Without this the wallpaper eats clicks meant for the desktop, so
        // rubber-band selection and right-click stop working.
        WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT
    };

    let holder = match *desktop {
        Layout::UnderIcons { progman, .. } if std::env::var_os("KIRIE_NO_LAYERED_HOST").is_none() => {
            layered_holder(progman, at, rect, style_ex)
        }
        _ => None,
    };
    let (parent, at) = match holder {
        Some(holder) => (holder, (0, 0)),
        None => (desktop.parent(), at),
    };

    // Made hidden, so that it is never seen in front of the icons while it is
    // still being put behind them.
    // SAFETY: the class is registered above, the parent is a live window, and
    // every pointer is either null or a null-terminated string that outlives
    // the call.
    let window = unsafe {
        CreateWindowExW(
            style_ex,
            wide(CLASS_NAME).as_ptr(),
            wide("kirie").as_ptr(),
            WS_CHILD | WS_CLIPSIBLINGS,
            at.0,
            at.1,
            rect.width() as i32,
            rect.height() as i32,
            parent,
            std::ptr::null_mut(),
            module_handle(),
            std::ptr::null(),
        )
    };
    if window.is_null() {
        tracing::error!("could not create the wallpaper window");
        if let Some(holder) = holder {
            destroy(holder);
        }
        return None;
    }

    let made = Made { window, holder };
    if let Layout::UnderIcons { icons, .. } = *desktop {
        under_icons(made.outer(), icons);
    }
    // SAFETY: showing windows we just made, without taking focus from whatever
    // the user is doing. The holder, when there is one, shows its child with it.
    unsafe {
        ShowWindow(window, SW_SHOWNA);
        if let Some(holder) = holder {
            ShowWindow(holder, SW_SHOWNA);
        }
    }
    Some(made)
}

/// A hidden, opaque, layered child of Progman over `rect`, or `None` when
/// Windows will not make one.
///
/// A layered child needs Windows 8 and an executable whose manifest says it
/// knows about Windows 8, which kirie's build embeds; without it Windows can
/// quietly make an ordinary window instead, so the style is read back. A
/// layered window also stays invisible until its opacity is set.
#[allow(unsafe_code)]
fn layered_holder(progman: Handle, at: (i32, i32), rect: Rect, style_ex: u32) -> Option<Handle> {
    // SAFETY: as for the wallpaper window in `desktop_window`.
    let holder = unsafe {
        CreateWindowExW(
            style_ex | WS_EX_LAYERED,
            wide(CLASS_NAME).as_ptr(),
            wide("kirie").as_ptr(),
            WS_CHILD | WS_CLIPSIBLINGS | WS_CLIPCHILDREN,
            at.0,
            at.1,
            rect.width() as i32,
            rect.height() as i32,
            progman,
            std::ptr::null_mut(),
            module_handle(),
            std::ptr::null(),
        )
    };
    if holder.is_null() {
        tracing::warn!("Windows would not make a layered window on the desktop; drawing without one");
        return None;
    }
    let layered = ex_style(holder) & WS_EX_LAYERED != 0;
    // SAFETY: a live window this process just made, with no pointer arguments.
    let opaque = layered && unsafe { SetLayeredWindowAttributes(holder, 0, 255, LWA_ALPHA) } != 0;
    if !opaque {
        tracing::warn!(
            layered,
            "Windows would not make a layered window on the desktop; drawing without one"
        );
        destroy(holder);
        return None;
    }
    Some(holder)
}

/// Put `window` directly below the icons, or at the top without them.
#[allow(unsafe_code)]
fn under_icons(window: Handle, icons: Option<Handle>) {
    // SAFETY: a z-order change with no pointer arguments; a dead handle on
    // either side makes it fail, and the next `keep_under_icons` tries again.
    unsafe {
        SetWindowPos(
            window,
            icons.unwrap_or(HWND_TOP),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

/// Keep `ours` below the icons and above Explorer's wallpaper layer on a
/// raised desktop, answering whether anything had to move.
///
/// Explorer makes its layer again whenever the wallpaper or the slideshow
/// changes, and a new window can land on top of ours, so the children are
/// read fresh each time, and only what is out of place is moved.
#[allow(unsafe_code)]
pub(crate) fn keep_under_icons(desktop: &mut Desktop, ours: &[Handle]) -> bool {
    let Layout::UnderIcons {
        progman,
        icons,
        layer,
    } = desktop
    else {
        return false;
    };
    let children = children(*progman);
    let classes: Vec<String> = children.iter().map(|child| class_of(*child)).collect();
    let first = |wanted: &str| {
        children
            .iter()
            .zip(&classes)
            .find(|(_, class)| class.as_str() == wanted)
            .map(|(child, _)| *child)
    };
    *icons = first(desktop_tree::ICONS);
    *layer = first(desktop_tree::WORKER);

    let fix = desktop_tree::restack(&children, ours, *icons, *layer);
    for window in &fix.lift {
        under_icons(*window, *icons);
    }
    if fix.sink_layer
        && let Some(layer) = *layer
    {
        // SAFETY: as in `under_icons`.
        unsafe {
            SetWindowPos(
                layer,
                HWND_BOTTOM,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }
    fix.is_needed()
}

/// Put `window` back inside `parent`.
///
/// This only moves the window between parents; on a raised desktop the caller
/// follows it with `keep_under_icons`, because a window given a parent lands on
/// top of its new siblings, in front of the icons.
#[allow(unsafe_code)]
pub(crate) fn reparent(window: Handle, parent: Handle) {
    if !alive(window) || !alive(parent) {
        return;
    }
    // SAFETY: both handles are live, checked immediately above.
    unsafe { SetParent(window, parent) };
}

/// Move and resize a wallpaper window to cover `rect`.
///
/// The z-order argument is null because `SWP_NOZORDER` is set, and that flag
/// tells Windows to ignore whatever is passed there. Keeping the order is what
/// is wanted: where a wallpaper sits among Explorer's windows is
/// `keep_under_icons`'s business, and a move is not a reason to change it.
#[allow(unsafe_code)]
pub(crate) fn place(made: Made, rect: Rect) {
    let origin = virtual_screen();
    let at = (
        rect.left.saturating_sub(origin.left),
        rect.top.saturating_sub(origin.top),
    );
    let resize = |window: Handle, (x, y): (i32, i32)| {
        if !alive(window) {
            return;
        }
        // SAFETY: a live window, and a placement with no pointer arguments.
        unsafe {
            SetWindowPos(
                window,
                std::ptr::null_mut(),
                x,
                y,
                rect.width() as i32,
                rect.height() as i32,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }
    };
    match made.holder {
        Some(holder) => {
            resize(holder, at);
            resize(made.window, (0, 0));
        }
        None => resize(made.window, at),
    }
}

#[allow(unsafe_code)]
pub(crate) fn destroy(window: Handle) {
    if !alive(window) {
        return;
    }
    // SAFETY: a live window this process created.
    unsafe { DestroyWindow(window) };
}

/// Deliver whatever messages have arrived for this thread's windows.
///
/// A wallpaper window has almost nothing to say, but a window whose queue is
/// never drained is a window Windows reports as hung.
#[allow(unsafe_code)]
pub(crate) fn pump_messages() {
    // SAFETY: MSG is a plain C struct with no invalid bit patterns, and
    // PeekMessageW overwrites it before anything reads it.
    let mut message: MSG = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: `message` is ours and lives across the call; a null window
        // filter asks for every message on this thread.
        let got = unsafe {
            PeekMessageW(
                std::ptr::from_mut(&mut message),
                std::ptr::null_mut(),
                0,
                0,
                PM_REMOVE,
            )
        };
        if got == 0 {
            break;
        }
        // SAFETY: dispatching a message the queue just handed us.
        unsafe { DispatchMessageW(std::ptr::from_ref(&message)) };
    }
}

/// Where the pointer is, in virtual-screen coordinates, and whether the left
/// button is down.
#[allow(unsafe_code)]
pub(crate) fn pointer() -> Option<((i32, i32), bool)> {
    let mut point = POINT { x: 0, y: 0 };
    // SAFETY: `point` is ours and lives across the call.
    let got = unsafe { GetCursorPos(std::ptr::from_mut(&mut point)) };
    if got == 0 {
        return None;
    }
    // SAFETY: a query for one virtual key's state.
    let held = unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON)) };
    Some(((point.x, point.y), held < 0))
}

/// The window the user is working in, if any, with its rectangle.
#[allow(unsafe_code)]
pub(crate) fn foreground_window() -> Option<(Handle, Rect)> {
    // SAFETY: a plain query returning null when nothing has focus.
    let window = unsafe { GetForegroundWindow() };
    if window.is_null() {
        return None;
    }
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: a live window and a rectangle that lives across the call.
    let got = unsafe { GetWindowRect(window, std::ptr::from_mut(&mut rect)) };
    if got == 0 {
        return None;
    }
    Some((window, Rect::from_win32(rect)))
}

/// The monitor a window is mostly on.
#[allow(unsafe_code)]
pub(crate) fn monitor_of(window: Handle) -> Option<Rect> {
    if !alive(window) {
        return None;
    }
    // SAFETY: a live window; the flag asks for the nearest monitor rather than
    // null when the window is off-screen.
    let monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_null() {
        return None;
    }
    // SAFETY: MONITORINFO is a plain C struct with no invalid bit patterns, and
    // cbSize is set to the size the API checks for immediately below.
    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<MONITORINFO>() as u32;
    // SAFETY: `info` is sized as the API requires and lives across the call.
    let ok = unsafe { GetMonitorInfoW(monitor, std::ptr::from_mut(&mut info)) };
    (ok != 0).then(|| Rect::from_win32(info.rcMonitor))
}

/// The file name of the program a window belongs to, lowercased, for matching
/// against the list of programs that should not pause the wallpaper.
///
/// This is what stands in for a Wayland `app_id`, which is what
/// `--fullscreen-pause-ignore` is written in terms of.
#[allow(unsafe_code)]
pub(crate) fn program_of(window: Handle) -> Option<String> {
    if !alive(window) {
        return None;
    }
    let mut pid: u32 = 0;
    // SAFETY: a live window and a u32 that lives across the call.
    unsafe { GetWindowThreadProcessId(window, std::ptr::from_mut(&mut pid)) };
    if pid == 0 {
        return None;
    }

    // SAFETY: opening a process for the least access that answers the question;
    // null on failure, which is checked.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return None;
    }

    // MAX_PATH is what nearly every program's path fits in, but a Win32 path
    // can be up to 32767 units long and the call answers a path that does not
    // fit with ERROR_INSUFFICIENT_BUFFER rather than with a truncated name.
    // Treating that as "no program" would quietly stop
    // `--fullscreen-pause-ignore` matching anything installed deep enough, so
    // grow the buffer once instead.
    let mut path = None;
    for capacity in [260_usize, 32_768] {
        let mut buffer = vec![0_u16; capacity];
        let mut len = capacity as u32;
        // SAFETY: the buffer and the length live across the call, and `len`
        // says how many units of the buffer may be written.
        let ok = unsafe {
            QueryFullProcessImageNameW(
                process,
                windows_sys::Win32::System::Threading::PROCESS_NAME_FORMAT::default(),
                buffer.as_mut_ptr(),
                std::ptr::from_mut(&mut len),
            )
        };
        if ok != 0 {
            path = Some(String::from_utf16_lossy(
                buffer.get(..len as usize).unwrap_or_default(),
            ));
            break;
        }
        // SAFETY: a thread-local error code, read right after the call that set
        // it and before anything else on this thread can overwrite it.
        let insufficient = unsafe { windows_sys::Win32::Foundation::GetLastError() }
            == windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
        if !insufficient {
            break;
        }
    }

    // SAFETY: a handle OpenProcess returned and nothing else holds.
    unsafe { windows_sys::Win32::Foundation::CloseHandle(process) };

    let path = path?;
    let file = path.rsplit(['\\', '/']).next()?;
    (!file.is_empty()).then(|| file.to_ascii_lowercase())
}

/// Whether the machine is running off its battery.
#[allow(unsafe_code)]
pub(crate) fn on_battery() -> bool {
    // SAFETY: SYSTEM_POWER_STATUS is a plain C struct of integers, and
    // GetSystemPowerStatus fills it before anything reads it.
    let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    // SAFETY: `status` is ours and lives across the call.
    let ok = unsafe { GetSystemPowerStatus(std::ptr::from_mut(&mut status)) };
    // 0 means running on battery, 1 means plugged in, 255 means unknown.
    ok != 0 && status.ACLineStatus == 0
}

const CLASS_NAME: &str = "kirie-wallpaper";

static CLASS_REGISTERED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

#[allow(unsafe_code)]
fn register_class() -> Option<()> {
    let registered = CLASS_REGISTERED.get_or_init(|| {
        // The name has to outlive the call, not just the expression that builds
        // the struct, so it is bound here rather than written inline.
        let name = wide(CLASS_NAME);
        let class = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(wallpaper_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: module_handle(),
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            // The renderer paints every pixel every frame, so letting Windows
            // clear the window first would only add a flash of colour.
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: name.as_ptr(),
        };
        // SAFETY: every pointer in `class` is null or points into `name`, which
        // lives until after RegisterClassW returns.
        let atom = unsafe { RegisterClassW(std::ptr::from_ref(&class)) };
        atom != 0
    });
    registered.then_some(())
}

#[allow(unsafe_code)]
unsafe extern "system" fn wallpaper_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_ERASEBKGND {
        // Claiming the erase stops Windows painting over the last frame while
        // the next one is still being drawn.
        return 1;
    }
    // SAFETY: handing a message we do not care about back to Windows.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

#[allow(unsafe_code)]
pub(crate) fn module_handle() -> windows_sys::Win32::Foundation::HMODULE {
    // SAFETY: a null name asks for this executable's own module, which always
    // exists.
    unsafe { GetModuleHandleW(std::ptr::null()) }
}

/// A UTF-16, null-terminated copy of `text`, for the `W` half of the Win32 API.
///
/// The caller keeps the returned vector alive for as long as the pointer it
/// takes from it is in use, which in this file is always a single call.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The handle wgpu needs, as a plain pointer.
pub(crate) fn as_raw(window: Handle) -> Option<std::ptr::NonNull<c_void>> {
    std::ptr::NonNull::new(window.cast())
}

/// A window's handle as a plain address, for code outside this crate that
/// needs the window but has its own Win32 bindings (a web page's view).
pub(crate) fn address(window: Handle) -> isize {
    window as isize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rect_measures_itself() {
        let rect = Rect {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1080,
        };
        assert_eq!(rect.width(), 1920);
        assert_eq!(rect.height(), 1080);
        assert!(rect.contains((-1, 1)));
        assert!(!rect.contains((0, 1)), "the right edge is outside");
        assert!(!rect.contains((-1, 1080)), "the bottom edge is outside");
    }

    #[test]
    fn an_empty_rect_still_measures_one_pixel() {
        // A zero-sized surface is not something wgpu will configure, so the
        // floor keeps a monitor that reports nothing from taking the run down.
        let rect = Rect {
            left: 5,
            top: 5,
            right: 5,
            bottom: 5,
        };
        assert_eq!((rect.width(), rect.height()), (1, 1));
    }

    #[test]
    fn a_device_path_becomes_a_screen_name() {
        let mut device = [0_u16; 32];
        for (slot, unit) in device.iter_mut().zip(r"\\.\DISPLAY2".encode_utf16()) {
            *slot = unit;
        }
        assert_eq!(display_name(&device, 1), "DISPLAY2");
    }

    #[test]
    fn a_nameless_device_falls_back_to_its_index() {
        assert_eq!(display_name(&[0_u16; 32], 3), "Screen-3");
    }

    #[test]
    fn a_string_going_to_win32_is_terminated() {
        assert_eq!(wide("ab"), vec![b'a' as u16, b'b' as u16, 0]);
    }

    // The tests below make real windows standing in for Explorer's, so that
    // the calls `desktop_tree`'s decisions go through are checked too: which
    // way round `GetWindow` lists children, where `SetWindowPos` puts a window,
    // and that a restack leaves the order it meant to.

    use windows_sys::Win32::UI::WindowsAndMessaging::WS_POPUP;

    /// A hidden window of `class`, registering the class on first use.
    #[allow(unsafe_code)]
    fn stand_in(class: &str, parent: Handle, style_ex: u32) -> Handle {
        let name = wide(class);
        let registration = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(wallpaper_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: module_handle(),
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: name.as_ptr(),
        };
        let style = if parent.is_null() { WS_POPUP } else { WS_CHILD };
        // SAFETY: as in `register_class` and `desktop_window`; registering a
        // class twice fails harmlessly, which the second test to get here does.
        let window = unsafe {
            RegisterClassW(std::ptr::from_ref(&registration));
            CreateWindowExW(
                style_ex,
                name.as_ptr(),
                name.as_ptr(),
                style,
                0,
                0,
                64,
                64,
                parent,
                std::ptr::null_mut(),
                module_handle(),
                std::ptr::null(),
            )
        };
        assert!(!window.is_null(), "could not make a stand-in {class}");
        window
    }

    #[allow(unsafe_code)]
    fn put_after(window: Handle, after: Handle) {
        // SAFETY: a z-order change between live windows, no pointer arguments.
        unsafe {
            SetWindowPos(
                window,
                after,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )
        };
    }

    const SMALL: Rect = Rect {
        left: 0,
        top: 0,
        right: 32,
        bottom: 32,
    };

    #[test]
    fn a_raised_desktop_gets_the_wallpaper_between_the_icons_and_the_layer() {
        let progman = stand_in(
            "kirie-test-progman",
            std::ptr::null_mut(),
            WS_EX_NOREDIRECTIONBITMAP,
        );
        let icons = stand_in(desktop_tree::ICONS, progman, 0);
        let layer = stand_in(desktop_tree::WORKER, progman, 0);
        put_after(layer, icons);
        assert_eq!(
            children(progman),
            vec![icons, layer],
            "children are listed topmost first"
        );

        let mut desktop = desktop_tree::choose(&snapshot_of(progman));
        assert_eq!(
            desktop,
            Layout::UnderIcons {
                progman,
                icons: Some(icons),
                layer: Some(layer)
            }
        );
        let made = desktop_window(&desktop, SMALL, false).expect("a wallpaper window");
        let ours = made.outer();
        assert_eq!(children(progman), vec![icons, ours, layer]);
        if let Some(holder) = made.holder {
            assert!(ex_style(holder) & WS_EX_LAYERED != 0, "the holder is layered");
            assert_eq!(children(holder), vec![made.window]);
        }

        // Explorer makes its layer again, and the new one lands on top.
        put_after(layer, HWND_TOP);
        assert_eq!(children(progman), vec![layer, icons, ours]);
        assert!(
            keep_under_icons(&mut desktop, &[ours]),
            "the layer was out of place"
        );
        assert_eq!(children(progman), vec![icons, ours, layer]);
        assert!(
            !keep_under_icons(&mut desktop, &[ours]),
            "nothing moves once in place"
        );

        // And a window that ends up over the icons goes back under them.
        put_after(ours, HWND_TOP);
        assert!(keep_under_icons(&mut desktop, &[ours]));
        assert_eq!(children(progman), vec![icons, ours, layer]);

        destroy(progman);
        assert!(!made.alive(), "children go with their parent");
    }

    #[test]
    fn a_classic_desktop_gets_the_wallpaper_in_the_worker_behind_the_icons() {
        let front = stand_in(desktop_tree::WORKER, std::ptr::null_mut(), 0);
        let _icons = stand_in(desktop_tree::ICONS, front, 0);
        let behind = stand_in(desktop_tree::WORKER, std::ptr::null_mut(), 0);
        put_after(behind, front);
        let progman = stand_in("kirie-test-progman", std::ptr::null_mut(), 0);

        let desktop = desktop_tree::choose(&snapshot_of(progman));
        assert_eq!(desktop, Layout::Behind { worker: behind });
        let made = desktop_window(&desktop, SMALL, false).expect("a wallpaper window");
        assert!(
            made.holder.is_none(),
            "only a raised desktop needs a layered window"
        );
        assert_eq!(children(behind), vec![made.window]);

        for window in [front, behind, progman] {
            destroy(window);
        }
    }
}
