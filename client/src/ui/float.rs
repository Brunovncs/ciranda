//! Windows that float over everything without taking a click or the focus: the in-game overlay
//! and the drawings over a screen you share. Windows is asked to leave them out of screen
//! captures, so a share shows neither, and nothing but Ciranda's own drawing is in them.
//!
//! Places are in the screen's physical pixels, as Windows gives monitors and windows. They hold
//! over games in a window or borderless full screen; a game in exclusive full screen owns the
//! display, and nothing that is not drawn inside it shows there.

use gpui::{
    App, Bounds, Entity, Render, Window, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions, point, px, size,
};

/// A rectangle of the screen, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Opens a floating window; `place` puts it where it belongs once open.
///
/// Windows answers a style or a move of a window by calling into it at once, which GPUI can't
/// take while that window is being updated: these calls are made outside any update, from the
/// window's handle, or GPUI would never hear of its new size.
pub fn open<V: Render + 'static>(cx: &mut App, build: impl FnOnce(&mut Window, &mut App) -> Entity<V>) -> Option<WindowHandle<V>> {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size: size(px(1.), px(1.)) })),
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    };
    match cx.open_window(options, build) {
        Ok(handle) => {
            if let Ok(Some(hwnd)) = handle.update(cx, |_, window, _| crate::ui::tray::hwnd(window)) {
                imp::float(hwnd);
            }
            Some(handle)
        }
        Err(e) => {
            log::warn!("no floating window: {e}");
            None
        }
    }
}

/// Puts the window `hwnd` at `rect`, above everything, again: another always-on-top window may
/// have come over it, and a move across monitors with another scale makes Windows resize it.
/// Not from inside an update of that window (see `open`).
pub fn place(hwnd: isize, rect: Rect) {
    imp::place(hwnd, rect);
}

/// The window's handle for `place`.
pub fn hwnd(window: &Window) -> Option<isize> {
    crate::ui::tray::hwnd(window)
}

pub use imp::{foreground_monitor, screen_rect, window_rect};

#[cfg(windows)]
mod imp {
    use super::Rect;
    use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, RECT};
    use windows::Win32::Graphics::Dwm::{
        DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DwmSetWindowAttribute,
    };
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, HWND_TOPMOST, IsIconic, IsWindow, LWA_ALPHA, SWP_NOACTIVATE,
        SWP_SHOWWINDOW, SetLayeredWindowAttributes, SetWindowDisplayAffinity, SetWindowLongPtrW, SetWindowPos, WDA_EXCLUDEFROMCAPTURE,
        WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
    };
    use windows::core::BOOL;

    fn rect(r: RECT) -> Rect {
        Rect { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top }
    }

    /// Clicks go through it to what is under, it never takes the focus, and captures skip it.
    pub fn float(hwnd: isize) {
        let h = HWND(hwnd as _);
        // SAFETY: a window of this process, alive while its GPUI window is.
        unsafe {
            let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
            let more = (WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST).0 as isize;
            SetWindowLongPtrW(h, GWL_EXSTYLE, ex | more);
            // Layered and fully opaque: what is drawn stays as drawn, the transparent rest lets
            // the clicks through.
            let _ = SetLayeredWindowAttributes(h, COLORREF(0), 255, LWA_ALPHA);
            // Windows 11 outlines and rounds top-level windows, a white line around the overlay
            // otherwise. Older versions don't know these and say so, which changes nothing.
            let none = DWMWA_COLOR_NONE;
            let _ = DwmSetWindowAttribute(h, DWMWA_BORDER_COLOR, &none as *const u32 as _, size_of::<u32>() as u32);
            let square = DWMWCP_DONOTROUND;
            let _ = DwmSetWindowAttribute(h, DWMWA_WINDOW_CORNER_PREFERENCE, &square as *const _ as _, size_of_val(&square) as u32);
            // A debug build can leave them in captures, for screenshots of them.
            let seen = cfg!(debug_assertions) && std::env::var_os("CIRANDA_FLOAT_CAPTURED").is_some();
            if !seen && let Err(e) = SetWindowDisplayAffinity(h, WDA_EXCLUDEFROMCAPTURE) {
                log::info!("floating window stays in captures: {e}");
            }
        }
    }

    pub fn place(hwnd: isize, r: Rect) {
        // SAFETY: as above.
        unsafe {
            let _ = SetWindowPos(HWND(hwnd as _), Some(HWND_TOPMOST), r.x, r.y, r.w.max(1), r.h.max(1), SWP_NOACTIVATE | SWP_SHOWWINDOW);
        }
    }

    fn monitor(m: HMONITOR) -> Option<(Rect, Rect)> {
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        // SAFETY: `info` is sized for the call.
        unsafe { GetMonitorInfoW(m, &mut info).as_bool() }.then(|| (rect(info.rcMonitor), rect(info.rcWork)))
    }

    /// The monitor of whatever has the focus (a game, usually): all of it, and its work area.
    pub fn foreground_monitor() -> Option<(Rect, Rect)> {
        // SAFETY: plain queries.
        monitor(unsafe { MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY) })
    }

    fn monitors() -> Vec<HMONITOR> {
        unsafe extern "system" fn each(m: HMONITOR, _: HDC, _: *mut RECT, out: LPARAM) -> BOOL {
            // SAFETY: `out` is the Vec passed below, alive for the whole enumeration.
            unsafe { (*(out.0 as *mut Vec<HMONITOR>)).push(m) };
            true.into()
        }
        let mut out: Vec<HMONITOR> = Vec::new();
        // SAFETY: the callback only pushes to `out`.
        unsafe {
            let _ = EnumDisplayMonitors(None, None, Some(each), LPARAM(&mut out as *mut _ as isize));
        }
        out
    }

    /// A shared screen's place, from the capturer's id for it: the monitor's handle when the
    /// capture is Windows Graphics Capture's, its position in the list when it is DirectX's.
    pub fn screen_rect(id: u64) -> Option<Rect> {
        let all = monitors();
        let m = all.iter().find(|m| m.0 as u64 == id).copied().or_else(|| all.get(id as usize).copied())?;
        monitor(m).map(|(full, _)| full)
    }

    /// A shared window's place; none once it is gone or minimised.
    pub fn window_rect(id: u64) -> Option<Rect> {
        let h = HWND(id as _);
        let mut r = RECT::default();
        // SAFETY: an HWND from the capturer; Windows checks it.
        unsafe { (IsWindow(Some(h)).as_bool() && !IsIconic(h).as_bool() && GetWindowRect(h, &mut r).is_ok()).then(|| rect(r)) }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::Rect;
    pub fn float(_: isize) {}
    pub fn place(_: isize, _: Rect) {}
    pub fn foreground_monitor() -> Option<(Rect, Rect)> {
        None
    }
    pub fn screen_rect(_: u64) -> Option<Rect> {
        None
    }
    pub fn window_rect(_: u64) -> Option<Rect> {
        None
    }
}
