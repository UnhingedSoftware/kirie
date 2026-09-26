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
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EnumWindows, FindWindowExW,
    FindWindowW, GetCursorPos, GetForegroundWindow, GetSystemMetrics, GetWindowRect,
    GetWindowThreadProcessId, IsWindow, MSG, PM_REMOVE, PeekMessageW, RegisterClassW, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SMTO_NORMAL, SW_SHOWNA, SWP_NOACTIVATE,
    SWP_NOZORDER, SendMessageTimeoutW, SetParent, SetWindowPos, ShowWindow, WM_ERASEBKGND, WNDCLASSW,
    WS_CHILD, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_VISIBLE,
};

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

/// The window to put wallpapers inside, which is the same one Wallpaper Engine
/// and Lively use.
///
/// The desktop is drawn by Explorer's `Progman`, and behind the icons there is
/// room for exactly one more window. Progman only creates that window --
/// a `WorkerW` -- when it is asked, and the way to ask is an undocumented
/// message, `0x052C`, that has been the way since Windows 7. Once it exists,
/// Explorer has two `WorkerW` windows: the one holding `SHELLDLL_DefView`,
/// which is the icons, and a sibling behind it, which is ours to draw in.
///
/// Every part of this can fail on a Windows that changed its mind -- the
/// message can do nothing, the sibling can be absent -- so the fallback is
/// Progman itself. Drawing there works; it just puts the wallpaper in front of
/// the icons rather than behind them.
#[allow(unsafe_code)]
pub(crate) fn wallpaper_host() -> Option<Handle> {
    // SAFETY: FindWindowW takes a null-terminated class name and no parent.
    let progman = unsafe { FindWindowW(wide("Progman").as_ptr(), std::ptr::null()) };
    if progman.is_null() {
        tracing::warn!("no Progman window; is Explorer running?");
        return None;
    }

    // SAFETY: a message send with a timeout, to a window we just found, whose
    // result we do not read.
    unsafe {
        SendMessageTimeoutW(
            progman,
            SPAWN_WORKERW,
            WPARAM::default(),
            LPARAM::default(),
            SMTO_NORMAL,
            SPAWN_TIMEOUT_MS,
            std::ptr::null_mut(),
        );
    }

    let mut worker: Handle = std::ptr::null_mut();
    // SAFETY: the callback matches EnumWindows's signature, and the lparam is
    // the address of `worker`, which outlives the call.
    unsafe {
        EnumWindows(Some(find_worker), std::ptr::from_mut(&mut worker) as LPARAM);
    }

    if worker.is_null() {
        tracing::warn!(
            "Explorer did not make a window behind the desktop icons; drawing in front of them instead"
        );
        return Some(progman);
    }
    Some(worker)
}

/// The message that makes Progman split the desktop in two. Undocumented, and
/// the same value every Windows since 7 has answered to.
const SPAWN_WORKERW: u32 = 0x052C;

const SPAWN_TIMEOUT_MS: u32 = 1_000;

#[allow(unsafe_code)]
unsafe extern "system" fn find_worker(window: HWND, out: LPARAM) -> i32 {
    // SAFETY: `window` comes from EnumWindows and is valid for this call.
    let shell_view = unsafe {
        FindWindowExW(
            window,
            std::ptr::null_mut(),
            wide("SHELLDLL_DefView").as_ptr(),
            std::ptr::null(),
        )
    };
    if shell_view.is_null() {
        return 1;
    }

    // The icons live here, so the window we want is the next WorkerW after it.
    // SAFETY: searching top-level windows starting after `window`.
    let sibling = unsafe {
        FindWindowExW(
            std::ptr::null_mut(),
            window,
            wide("WorkerW").as_ptr(),
            std::ptr::null(),
        )
    };
    if sibling.is_null() {
        return 1;
    }

    // SAFETY: the lparam is the `&mut Handle` wallpaper_host() passed in.
    unsafe { *(out as *mut Handle) = sibling };
    0
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

/// Make a borderless child window covering `rect`, parented into the desktop.
///
/// `rect` is in virtual-screen coordinates; the host's client area starts at
/// the virtual screen's own origin, so the offset between the two is what turns
/// one into the other.
#[allow(unsafe_code)]
pub(crate) fn desktop_window(host: Handle, rect: Rect, take_clicks: bool) -> Option<Handle> {
    register_class()?;

    let origin = virtual_screen();
    let style_ex = if take_clicks {
        WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW
    } else {
        // Without this the wallpaper eats clicks meant for the desktop, so
        // rubber-band selection and right-click stop working.
        WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT
    };

    // SAFETY: the class is registered above, the parent is a live window, and
    // every pointer is either null or a null-terminated string that outlives
    // the call.
    let window = unsafe {
        CreateWindowExW(
            style_ex,
            wide(CLASS_NAME).as_ptr(),
            wide("kirie").as_ptr(),
            WS_CHILD | WS_VISIBLE,
            rect.left.saturating_sub(origin.left),
            rect.top.saturating_sub(origin.top),
            rect.width() as i32,
            rect.height() as i32,
            host,
            std::ptr::null_mut(),
            module_handle(),
            std::ptr::null(),
        )
    };
    if window.is_null() {
        tracing::error!("could not create the wallpaper window");
        return None;
    }

    // CreateWindowExW already took `host` as the parent, but Explorer restarting
    // orphans the window, and re-parenting is how it is put back; doing it here
    // too keeps one path for both.
    reparent(window, host);
    // SAFETY: showing a window we just made, without taking focus from whatever
    // the user is doing.
    unsafe { ShowWindow(window, SW_SHOWNA) };
    Some(window)
}

/// Put `window` back inside `host` and over `rect`.
#[allow(unsafe_code)]
pub(crate) fn reparent(window: Handle, host: Handle) {
    if !alive(window) || !alive(host) {
        return;
    }
    // SAFETY: both handles are live, checked immediately above.
    unsafe { SetParent(window, host) };
}

/// Move and resize a wallpaper window to cover `rect`.
///
/// The z-order argument is null because `SWP_NOZORDER` is set, and that flag
/// tells Windows to ignore whatever is passed there. Keeping the order is what
/// is wanted: the wallpaper windows are children of the desktop host, one per
/// monitor, so they never overlap each other, and sinking one to the bottom of
/// that sibling list would gain nothing while risking a reshuffle every poll.
#[allow(unsafe_code)]
pub(crate) fn place(window: Handle, rect: Rect) {
    if !alive(window) {
        return;
    }
    let origin = virtual_screen();
    // SAFETY: a live window, and a placement with no pointer arguments.
    unsafe {
        SetWindowPos(
            window,
            std::ptr::null_mut(),
            rect.left.saturating_sub(origin.left),
            rect.top.saturating_sub(origin.top),
            rect.width() as i32,
            rect.height() as i32,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
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
}
