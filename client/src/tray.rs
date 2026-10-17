//! The notification-area icon, and one Ciranda at a time. While the icon is there, closing the
//! window only hides it: the session and any call go on, a click on the icon brings the window
//! back and its menu quits for real. As in OpenController, starting Ciranda again shows the
//! running copy's window instead; "one" is per settings folder, so copies started with their own
//! `CIRANDA_DATA_DIR` still run side by side.
//!
//! Plain Win32. The icon lives on a thread of its own with a hidden window for the shell's
//! messages, so its menu, which runs a modal loop until it closes, never holds up the window.
//! Elsewhere than Windows there is no icon and closing the window quits; one Ciranda at a time
//! holds there too.
//!
//! In a call the icon says how you are in it, as Discord's does, and the window's thumbnail on the
//! taskbar gets buttons to mute, deafen and leave (`set_thumbs`), so neither needs the window.

/// What the icon, the taskbar's buttons and a second start ask of the window.
pub enum Event {
    /// A click on the icon or on its notice, "Open" in its menu, or a second start.
    Open,
    /// A right click on the icon: the menu waits for `Tray::menu`.
    Menu,
    /// The pointer is over the icon, so its tooltip is about to show.
    Hover,
    Mute,
    Deafen,
    /// "Disconnect", from the menu or the thumbnail.
    Leave,
    /// The window has a new taskbar button, which has none of the thumbnail's buttons yet, or
    /// the taskbar changed between light and dark: they are to be put on again.
    Taskbar,
    Quit,
}

/// What the menu offers besides opening and quitting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Menu {
    /// In a call: whether muted, and whether deafened.
    pub call: Option<(bool, bool)>,
}

/// You in a call, as the icon and the thumbnail's buttons show it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InCall {
    pub muted: bool,
    pub deafened: bool,
    pub speaking: bool,
}

/// What the icon shows: Ciranda's own outside a call; in one a grey tile that turns green while
/// you talk, with a red badge while muted or deafened.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Look {
    #[default]
    Idle,
    Call,
    Speaking,
    Muted,
    Deafened,
}

impl Look {
    /// Deafened is muted too, and says the more; muted, nobody hears you talk.
    pub fn of(call: Option<InCall>) -> Look {
        match call {
            None => Look::Idle,
            Some(c) if c.deafened => Look::Deafened,
            Some(c) if c.muted => Look::Muted,
            Some(c) if c.speaking => Look::Speaking,
            Some(_) => Look::Call,
        }
    }
}

/// The thumbnail's buttons in a call: mute, deafen, disconnect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Thumbs {
    pub muted: bool,
    pub deafened: bool,
    /// The taskbar is dark, so the buttons are drawn light.
    pub dark: bool,
}

impl Thumbs {
    /// Each button's picture and colour. One that is "on" (muted, deafened) is red on either
    /// taskbar, as the window's own mute button is.
    fn pictures(self) -> [(&'static str, &'static str); 3] {
        let ink = if self.dark { pictures::INK_DARK } else { pictures::INK_LIGHT };
        [
            if self.muted { ("mic-off", pictures::RED) } else { ("mic", ink) },
            if self.deafened { ("headphones-off", pictures::RED) } else { ("headphones", ink) },
            ("phone-off", ink),
        ]
    }
}

/// The pictures the icon and the buttons show, made here as SVG and drawn by GPUI's own SVG
/// renderer at the size the system asks for. The tile is the app icon's, recoloured: five bars,
/// drawn thicker than the icon's so they still read at 16 px.
#[cfg_attr(not(windows), allow(dead_code))]
mod pictures {
    use super::Look;

    pub const GREY: &str = "#80848e";
    pub const GREEN: &str = "#23a55a";
    pub const RED: &str = "#f23f43";
    pub const INK_DARK: &str = "#f2f3f5";
    pub const INK_LIGHT: &str = "#313338";

    /// One of the interface's icons (`crate::icons`), `size` across at `(x, y)`, in `color`, with
    /// strokes `stroke` wide on its own 24 grid: small pictures need them thicker.
    fn glyph(name: &str, color: &str, x: f32, y: f32, size: f32, stroke: f32) -> String {
        format!(
            r#"<g transform="translate({x} {y}) scale({s})" fill="none" stroke="{color}" stroke-width="{stroke}" stroke-linecap="round" stroke-linejoin="round">{shapes}</g>"#,
            s = size / 24.,
            shapes = crate::icons::shapes(name),
        )
    }

    /// The icon for `look` on a 64 grid, or `None` for Ciranda's own.
    pub fn tray_svg(look: Look) -> Option<String> {
        let (tile, badge) = match look {
            Look::Idle => return None,
            Look::Call => (GREY, None),
            Look::Speaking => (GREEN, None),
            Look::Muted => (GREY, Some("mic-off")),
            Look::Deafened => (GREY, Some("headphones-off")),
        };
        let (w, gap) = (6.4f32, 4.2f32);
        let left = (64. - (5. * w + 4. * gap)) / 2.;
        let bars: String = [0.2f32, 0.4, 0.58, 0.4, 0.2]
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let (x, h) = (left + i as f32 * (w + gap), h * 64.);
                format!(r##"<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{r}" fill="#fff"/>"##, y = (64. - h) / 2., r = w / 2.)
            })
            .collect();
        let tile = format!(r#"<rect width="64" height="64" rx="14" fill="{tile}"/>{bars}"#);
        let body = match badge {
            None => tile,
            // A round badge in the bottom corner, cut out of the tile with a gap around it so it
            // stays a shape of its own at 16 px.
            Some(name) => {
                let (r, c) = (21f32, 64. - 21.);
                format!(
                    r##"<defs><mask id="cut"><rect width="64" height="64" fill="#fff"/><circle cx="{c}" cy="{c}" r="{gap}" fill="#000"/></mask></defs><g mask="url(#cut)">{tile}</g><circle cx="{c}" cy="{c}" r="{r}" fill="{RED}"/>{glyph}"##,
                    gap = r + 4.,
                    glyph = glyph(name, "#fff", c - r * 0.7, c - r * 0.7, r * 1.4, 3.6),
                )
            }
        };
        Some(format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" width="64" height="64">{body}</svg>"#))
    }

    /// A thumbnail button's picture: the icon alone, in `color`.
    pub fn button_svg(name: &str, color: &str) -> String {
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24">{}</svg>"#,
            glyph(name, color, 0., 0., 24., 2.25)
        )
    }

    /// `svg` drawn `size` pixels square: rows from the top, each pixel blue, green, red and alpha
    /// (not premultiplied), as an icon's colour bitmap takes them.
    pub fn paint(svg: &str, size: u32) -> Option<Vec<u8>> {
        let renderer = gpui::SvgRenderer::new(std::sync::Arc::new(()));
        let tree = renderer.parse_svg(svg.as_bytes()).ok()?;
        let px = gpui::DevicePixels(size as i32);
        let image = renderer.render_parsed(&tree, gpui::SvgSize::ExactSize(gpui::size(px, px))).ok()?;
        image.as_bytes(0).map(<[u8]>::to_vec)
    }
}

pub use platform::{
    Instance, Tray, dark_taskbar, flash_window, forget_thumbs, hide_window, hook_taskbar, is_minimized, quit_running, set_thumbs,
    show_window,
};

#[cfg(windows)]
mod platform {
    use super::{Event, Look, Menu, Thumbs, pictures};
    use parking_lot::Mutex;
    use sha2::{Digest, Sha256};
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WAIT_ABANDONED, WAIT_OBJECT_0, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::{
        CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, OpenMutexW, ReleaseMutex, SYNCHRONIZATION_SYNCHRONIZE,
        SetEvent, WaitForMultipleObjects, WaitForSingleObject,
    };
    use windows::Win32::UI::Shell::{
        NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_USER, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION,
        NIN_BALLOONUSERCLICK, NIN_SELECT, NINF_KEY, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        ASFW_ANY, AllowSetForegroundWindow, AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
        DispatchMessageW, GetMessageW, GetSystemMetrics, HICON, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW, MENU_ITEM_FLAGS, MF_CHECKED,
        MF_SEPARATOR, MF_STRING, MSG, PostMessageW, RegisterClassW, RegisterWindowMessageW, SM_CXSMICON, SM_CYSMICON, SW_HIDE, SW_SHOW,
        SetForegroundWindow, SetMenuDefaultItem, ShowWindowAsync, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx,
        TranslateMessage, WM_APP, WM_CONTEXTMENU, WM_MOUSEMOVE, WM_NULL, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
    };
    use windows::core::{PCWSTR, w};

    /// The shell's messages about the icon.
    const CALLBACK: u32 = WM_APP + 1;
    /// "Show `pending.tip`", "pop up the menu `pending.menu` describes", "show `pending.notice`",
    /// "look as `pending.look` says".
    const SET_TIP: u32 = WM_APP + 2;
    const POPUP: u32 = WM_APP + 3;
    const NOTICE: u32 = WM_APP + 4;
    const SET_LOOK: u32 = WM_APP + 5;
    const ICON_ID: u32 = 1;
    /// The icon chosen with the keyboard and Enter; the crate has no name for it.
    const NIN_KEYSELECT: u32 = NIN_SELECT | NINF_KEY;
    /// The menu's commands.
    const OPEN: usize = 1;
    const MUTE: usize = 2;
    const DEAFEN: usize = 3;
    const QUIT: usize = 4;
    const LEAVE: usize = 5;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    /// Copies `s` into one of the shell's fixed buffers, cut to fit.
    fn fill(buf: &mut [u16], s: &str) {
        let room = buf.len() - 1;
        let mut n = 0;
        for (slot, unit) in buf.iter_mut().zip(s.encode_utf16().take(room)) {
            *slot = unit;
            n += 1;
        }
        buf[n] = 0;
    }

    /// The settings folder's claim: a mutex this (the main) thread holds until Ciranda exits,
    /// and the events another start signals to have the window shown, or Ciranda quit.
    pub struct Instance {
        show: Option<isize>,
        quit: Option<isize>,
    }

    /// `Local\Ciranda-<hash of the settings folder>`, the mutex's name; the events add a suffix.
    fn instance_name(app: &str, data_dir: &Path) -> String {
        let hash = hex::encode(Sha256::digest(data_dir.to_string_lossy().to_lowercase().as_bytes()));
        format!(r"Local\{app}-{}", &hash[..16])
    }

    /// Asks the Ciranda running on this settings folder to quit, as its menu's Quit would, and
    /// waits up to `timeout` for it to be gone; false when it is still there. The installer
    /// runs this (`--quit`) before it replaces or removes the program.
    pub fn quit_running(data_dir: &Path, timeout: Duration) -> bool {
        let name = instance_name("Ciranda", data_dir);
        let (mutex, quit) = (wide(&name), wide(&format!("{name}-quit")));
        // SAFETY: plain Win32 calls with nul-terminated names that outlive them; every handle
        // opened here is closed when the process exits, moments later.
        unsafe {
            let Ok(m) = OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, PCWSTR(mutex.as_ptr())) else { return true };
            if let Ok(ev) = OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(quit.as_ptr())) {
                let _ = SetEvent(ev);
            }
            let got = WaitForSingleObject(m, timeout.as_millis().min(u32::MAX as u128) as u32);
            let gone = got == WAIT_OBJECT_0 || got == WAIT_ABANDONED;
            if gone {
                let _ = ReleaseMutex(m);
            }
            gone
        }
    }

    impl Instance {
        /// `None` when another Ciranda runs on this settings folder: it has been asked to show its
        /// window and this one should exit. After an update restarts Ciranda (`wait`), the copy
        /// being replaced is given time to finish quitting first.
        pub fn claim(data_dir: &Path, wait: bool) -> Option<Instance> {
            Instance::claim_as("Ciranda", data_dir, wait)
        }

        /// The claim a Harmony up to 4.1.1 holds on its folder, so the folder is moved only once
        /// that copy has quit: after the update to Ciranda it is still quitting as this one starts.
        pub fn claim_legacy(data_dir: &Path, wait: bool) -> Option<Instance> {
            Instance::claim_as("Harmony", data_dir, wait)
        }

        fn claim_as(app: &str, data_dir: &Path, wait: bool) -> Option<Instance> {
            let name = instance_name(app, data_dir);
            let (mutex, show, quit) = (wide(&name), wide(&format!("{name}-show")), wide(&format!("{name}-quit")));
            // SAFETY: plain Win32 calls with nul-terminated names that outlive them. The handles
            // stay open for the life of the process.
            unsafe {
                let Ok(m) = CreateMutexW(None, false, PCWSTR(mutex.as_ptr())) else {
                    log::warn!("single instance: no mutex, running anyway");
                    return Some(Instance { show: None, quit: None });
                };
                // An abandoned mutex is one whose holder exited without letting go: it is ours.
                let got = WaitForSingleObject(m, if wait { 15_000 } else { 0 });
                if got != WAIT_OBJECT_0 && got != WAIT_ABANDONED {
                    if let Ok(ev) = OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(show.as_ptr())) {
                        // The running copy may take the foreground, which this start has from the
                        // click that launched it.
                        let _ = AllowSetForegroundWindow(ASFW_ANY);
                        let _ = SetEvent(ev);
                    }
                    return None;
                }
                let event = |name: &[u16]| CreateEventW(None, false, false, PCWSTR(name.as_ptr())).ok().map(|h| h.0 as isize);
                Some(Instance { show: event(&show), quit: event(&quit) })
            }
        }

        /// Sends `Event::Open` each time another start asks for the window, and `Event::Quit`
        /// when `--quit` asks Ciranda to go.
        pub fn on_show(&self, events: async_channel::Sender<Event>) {
            // Each event, and whether it is the one asking to quit.
            let events_of: Vec<(isize, bool)> = [(self.show, false), (self.quit, true)].into_iter().filter_map(|(h, q)| Some((h?, q))).collect();
            if events_of.is_empty() {
                return;
            }
            let spawned = std::thread::Builder::new().name("ciranda-instance".into()).spawn(move || {
                let handles: Vec<HANDLE> = events_of.iter().map(|&(h, _)| HANDLE(h as _)).collect();
                loop {
                    // SAFETY: the events stay open for the life of the process.
                    let got = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
                    let Some(&(_, quit)) = events_of.get(got.0.wrapping_sub(WAIT_OBJECT_0.0) as usize) else { break };
                    if events.send_blocking(if quit { Event::Quit } else { Event::Open }).is_err() {
                        break;
                    }
                }
            });
            if spawned.is_err() {
                log::warn!("single instance: no thread to hear other starts");
            }
        }
    }

    /// What the window asked for last, read by the icon's thread when the message saying so
    /// arrives.
    #[derive(Default)]
    struct Pending {
        tip: String,
        menu: Menu,
        notice: (String, String),
        look: Look,
    }

    struct Shared {
        events: async_channel::Sender<Event>,
        pending: Mutex<Pending>,
        icon: isize,
        /// Sent to every top-level window when Explorer starts again, which forgets every icon.
        taskbar_created: u32,
    }

    static SHARED: OnceLock<Shared> = OnceLock::new();

    thread_local! {
        /// Where the shell asked for the menu: the pointer, or the icon when asked from the keyboard.
        static ANCHOR: Cell<(i32, i32)> = const { Cell::new((0, 0)) };
        static HOVERED: Cell<Option<Instant>> = const { Cell::new(None) };
        /// The icon in a call, one per look, drawn the first time it is needed and kept: talking
        /// flips between two of them several times a second.
        static LOOKS: RefCell<HashMap<Look, isize>> = RefCell::new(HashMap::new());
    }

    /// The notification area's icon size: 16 px at 100%, more on a denser screen.
    fn small_icon() -> i32 {
        // SAFETY: a plain query.
        unsafe { GetSystemMetrics(SM_CXSMICON) }
    }

    /// An icon from `paint`'s pixels, `size` square; 0 when Windows would not make it.
    fn make_icon(bgra: &[u8], size: i32) -> isize {
        use windows::Win32::Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS, DeleteObject,
        };
        use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, ICONINFO};
        if size <= 0 || bgra.len() != (size * size * 4) as usize {
            return 0;
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                // Negative: rows from the top, as `paint` gives them.
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        // The colour bitmap's alpha is what is used; the mask, all zeros, only has to be there.
        let mask = vec![0u8; (size as usize).div_ceil(16) * 2 * size as usize];
        let mut bits = std::ptr::null_mut();
        // SAFETY: `bits` is the section's own buffer, `size` rows of `size` 4-byte pixels, which
        // `bgra` was checked to fill exactly; the bitmaps are copied into the icon and let go.
        unsafe {
            let Ok(color) = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0) else { return 0 };
            if bits.is_null() {
                let _ = DeleteObject(color.into());
                return 0;
            }
            std::ptr::copy_nonoverlapping(bgra.as_ptr(), bits.cast::<u8>(), bgra.len());
            let mask = CreateBitmap(size, size, 1, 1, Some(mask.as_ptr().cast()));
            let icon = CreateIconIndirect(&ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color });
            let _ = DeleteObject(color.into());
            let _ = DeleteObject(mask.into());
            icon.map_or(0, |h| h.0 as isize)
        }
    }

    /// The icon `look` asks for: Ciranda's own outside a call, or when a drawn one would not come.
    fn icon_for(look: Look, s: &Shared) -> HICON {
        let drawn = LOOKS.with(|looks| {
            *looks.borrow_mut().entry(look).or_insert_with(|| {
                let size = small_icon();
                pictures::tray_svg(look).and_then(|svg| pictures::paint(&svg, size as u32)).map_or(0, |px| make_icon(&px, size))
            })
        });
        HICON(if drawn != 0 { drawn } else { s.icon } as _)
    }

    /// The icon's thread and window.
    pub struct Tray {
        hwnd: isize,
    }

    impl Tray {
        pub fn start(events: async_channel::Sender<Event>) -> Option<Tray> {
            let (ready, hwnd) = std::sync::mpsc::channel();
            std::thread::Builder::new().name("ciranda-tray".into()).spawn(move || run(events, ready)).ok()?;
            hwnd.recv().ok().flatten().map(|hwnd| Tray { hwnd })
        }

        fn post(&self, msg: u32, f: impl FnOnce(&mut Pending)) {
            if let Some(s) = SHARED.get() {
                f(&mut s.pending.lock());
            }
            // SAFETY: the icon's window lives as long as the process.
            if unsafe { PostMessageW(Some(HWND(self.hwnd as _)), msg, WPARAM(0), LPARAM(0)) }.is_err() {
                log::warn!("tray: the icon's thread did not take a message");
            }
        }

        pub fn set_tip(&self, tip: &str) {
            self.post(SET_TIP, |p| p.tip = tip.into());
        }

        /// How you are in a call, or `Look::Idle` outside one. Only call it when that changed.
        pub fn set_look(&self, look: Look) {
            self.post(SET_LOOK, |p| p.look = look);
        }

        /// Pops up the menu the last right click asked for.
        pub fn menu(&self, menu: Menu) {
            self.post(POPUP, |p| p.menu = menu);
        }

        /// A notification from the icon, which Windows shows as a toast.
        pub fn notify(&self, title: &str, text: &str) {
            self.post(NOTICE, |p| p.notice = (title.into(), text.into()));
        }

        /// Takes the icon away now: one left behind stays until the pointer passes over it.
        pub fn remove(&self) {
            // SAFETY: the shell is told by window and id; any thread may do so.
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &data(HWND(self.hwnd as _)));
            }
        }
    }

    pub fn hide_window(hwnd: isize) {
        // SAFETY: asynchronous, so the window's own procedure does not run inside the caller's
        // update of the app.
        unsafe {
            let _ = ShowWindowAsync(HWND(hwnd as _), SW_HIDE);
        }
    }

    pub fn show_window(hwnd: isize) {
        // SAFETY: as in `hide_window`.
        unsafe {
            let _ = ShowWindowAsync(HWND(hwnd as _), SW_SHOW);
        }
    }

    pub fn is_minimized(hwnd: isize) -> bool {
        // SAFETY: a plain query; a window that is gone answers false.
        unsafe { windows::Win32::UI::WindowsAndMessaging::IsIconic(HWND(hwnd as _)).as_bool() }
    }

    /// Flashes the window's taskbar button until it is brought to the front, as a call should.
    pub fn flash_window(hwnd: isize) {
        use windows::Win32::UI::WindowsAndMessaging::{FLASHW_ALL, FLASHW_TIMERNOFG, FLASHWINFO, FlashWindowEx};
        let info = FLASHWINFO {
            cbSize: size_of::<FLASHWINFO>() as u32,
            hwnd: HWND(hwnd as _),
            dwFlags: FLASHW_ALL | FLASHW_TIMERNOFG,
            uCount: 0,
            dwTimeout: 0,
        };
        // SAFETY: `info` is a complete FLASHWINFO for this window, read during the call only.
        unsafe {
            let _ = FlashWindowEx(&info);
        }
    }

    fn data(hwnd: HWND) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW { cbSize: size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: ICON_ID, ..Default::default() }
    }

    fn add(hwnd: HWND, s: &Shared) -> bool {
        let mut d = data(hwnd);
        d.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        d.uCallbackMessage = CALLBACK;
        let look = s.pending.lock().look;
        d.hIcon = icon_for(look, s);
        fill(&mut d.szTip, &s.pending.lock().tip);
        d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        // SAFETY: `d` is a complete NOTIFYICONDATAW for this window's icon.
        unsafe { Shell_NotifyIconW(NIM_ADD, &d).as_bool() && Shell_NotifyIconW(NIM_SETVERSION, &d).as_bool() }
    }

    fn run(events: async_channel::Sender<Event>, ready: std::sync::mpsc::Sender<Option<isize>>) {
        // SAFETY: plain Win32 calls on this thread's own window and queue; every pointer passed
        // outlives its call.
        unsafe {
            let Ok(module) = GetModuleHandleW(None) else {
                let _ = ready.send(None);
                return;
            };
            // Resource 1 is Ciranda's own icon, at the size the notification area draws.
            let icon = LoadImageW(
                Some(module.into()),
                PCWSTR(1 as _),
                IMAGE_ICON,
                GetSystemMetrics(SM_CXSMICON),
                GetSystemMetrics(SM_CYSMICON),
                LR_DEFAULTCOLOR,
            )
            .map_or(0, |h| h.0 as isize);
            let _ = SHARED.set(Shared {
                events,
                pending: Mutex::new(Pending { tip: "Ciranda".into(), ..Default::default() }),
                icon,
                taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            });
            let class = w!("CirandaTray");
            let wc = WNDCLASSW { lpfnWndProc: Some(window_proc), hInstance: module.into(), lpszClassName: class, ..Default::default() };
            RegisterClassW(&wc);
            // A real top-level window, never shown, rather than a message-only one: only those
            // hear that Explorer restarted.
            let hwnd = match CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class,
                w!("Ciranda tray"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(module.into()),
                None,
            ) {
                Ok(hwnd) => hwnd,
                Err(e) => {
                    log::warn!("tray: no window ({e})");
                    let _ = ready.send(None);
                    return;
                }
            };
            if icon == 0 || !SHARED.get().is_some_and(|s| add(hwnd, s)) {
                log::warn!("tray: the notification area did not take the icon");
                let _ = DestroyWindow(hwnd);
                let _ = ready.send(None);
                return;
            }
            let _ = ready.send(Some(hwnd.0 as isize));
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        let Some(s) = SHARED.get() else { return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) } };
        let send = |e| {
            let _ = s.events.try_send(e);
        };
        // SAFETY: the shell's structures are filled for this window's own icon.
        unsafe {
            match msg {
                // With version 4 the low word is what happened and the anchor is in wParam.
                CALLBACK => match (lparam.0 & 0xFFFF) as u32 {
                    NIN_SELECT | NIN_KEYSELECT | NIN_BALLOONUSERCLICK => send(Event::Open),
                    WM_CONTEXTMENU => {
                        ANCHOR.set(((wparam.0 & 0xFFFF) as i16 as i32, ((wparam.0 >> 16) & 0xFFFF) as i16 as i32));
                        send(Event::Menu);
                    }
                    // Plenty while the pointer rests there; once a second keeps the tooltip current.
                    WM_MOUSEMOVE if HOVERED.get().is_none_or(|t| t.elapsed() > Duration::from_secs(1)) => {
                        HOVERED.set(Some(Instant::now()));
                        send(Event::Hover);
                    }
                    _ => {}
                },
                SET_TIP => {
                    let mut d = data(hwnd);
                    d.uFlags = NIF_TIP | NIF_SHOWTIP;
                    fill(&mut d.szTip, &s.pending.lock().tip);
                    let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
                }
                SET_LOOK => {
                    let look = s.pending.lock().look;
                    let mut d = data(hwnd);
                    d.uFlags = NIF_ICON;
                    d.hIcon = icon_for(look, s);
                    let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
                }
                POPUP => popup(hwnd, s),
                NOTICE => {
                    let (title, text) = std::mem::take(&mut s.pending.lock().notice);
                    let mut d = data(hwnd);
                    d.uFlags = NIF_INFO;
                    fill(&mut d.szInfoTitle, &title);
                    fill(&mut d.szInfo, &text);
                    d.dwInfoFlags = NIIF_USER;
                    d.hBalloonIcon = HICON(s.icon as _);
                    let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
                }
                m if m == s.taskbar_created => {
                    add(hwnd, s);
                }
                _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
            }
        }
        LRESULT(0)
    }

    /// Runs the menu where the shell asked for it, and sends what was picked.
    fn popup(hwnd: HWND, s: &Shared) {
        let menu = s.pending.lock().menu;
        // SAFETY: the menu is made, shown and destroyed here, on the thread that owns `hwnd`.
        unsafe {
            let Ok(m) = CreatePopupMenu() else { return };
            let item = |flags: MENU_ITEM_FLAGS, id: usize, text: &str| {
                let text = wide(text);
                let _ = AppendMenuW(m, MF_STRING | flags, id, PCWSTR(text.as_ptr()));
            };
            let separator = || {
                let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
            };
            item(MENU_ITEM_FLAGS(0), OPEN, tr!("Open Ciranda", "Abrir Ciranda"));
            let _ = SetMenuDefaultItem(m, OPEN as u32, 0);
            if let Some((muted, deafened)) = menu.call {
                let check = |on: bool| if on { MF_CHECKED } else { MENU_ITEM_FLAGS(0) };
                separator();
                item(check(muted), MUTE, tr!("Mute", "Silenciar"));
                item(check(deafened), DEAFEN, tr!("Deafen", "Desativar o som"));
                item(MENU_ITEM_FLAGS(0), LEAVE, tr!("Disconnect", "Desconectar"));
            }
            separator();
            item(MENU_ITEM_FLAGS(0), QUIT, tr!("Quit Ciranda", "Fechar Ciranda"));
            // Without the foreground the menu would not close when clicking elsewhere.
            let _ = SetForegroundWindow(hwnd);
            let (x, y) = ANCHOR.get();
            let picked = TrackPopupMenuEx(m, (TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY).0, x, y, hwnd, None);
            // The menu closes properly on the next message the window gets.
            let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(m);
            let event = match picked.0 as usize {
                OPEN => Event::Open,
                MUTE => Event::Mute,
                DEAFEN => Event::Deafen,
                LEAVE => Event::Leave,
                QUIT => Event::Quit,
                _ => return,
            };
            let _ = s.events.try_send(event);
        }
    }

    /// What the window's own procedure is told before GPUI's: the thumbnail's buttons and the
    /// taskbar's news. Set once, from the main thread, which owns the window.
    struct Hook {
        events: async_channel::Sender<Event>,
        /// "TaskbarButtonCreated", sent when the window gets a taskbar button: at first, after
        /// being hidden in the tray, and when Explorer starts again.
        created: u32,
    }

    static HOOK: OnceLock<Hook> = OnceLock::new();
    const HOOK_ID: usize = 0xC1DA;
    /// The thumbnail's buttons, by id.
    const THUMB_MUTE: u32 = 1;
    const THUMB_DEAFEN: u32 = 2;
    const THUMB_LEAVE: u32 = 3;

    thread_local! {
        // On the main thread, the window's: the taskbar's COM object lives in its apartment.
        static TASKBAR: RefCell<Option<windows::Win32::UI::Shell::ITaskbarList3>> = const { RefCell::new(None) };
        /// The buttons are on the window's taskbar button, so changing them is an update. A
        /// taskbar button has them added once, and a new button has none.
        static THUMBS_ADDED: Cell<bool> = const { Cell::new(false) };
        /// Each button picture, by icon and colour, drawn once.
        static BUTTON_ICONS: RefCell<HashMap<(&'static str, &'static str), isize>> = RefCell::new(HashMap::new());
    }

    /// Hears the thumbnail's buttons on the window `hwnd`. On the main thread, before its message
    /// loop next runs, so the first "TaskbarButtonCreated" is not missed.
    pub fn hook_taskbar(hwnd: isize, events: async_channel::Sender<Event>) {
        use windows::Win32::UI::Shell::SetWindowSubclass;
        // SAFETY: the window is this thread's, and `subclass` stays valid for the process.
        unsafe {
            let created = RegisterWindowMessageW(w!("TaskbarButtonCreated"));
            if HOOK.set(Hook { events, created }).is_err() {
                return;
            }
            if !SetWindowSubclass(HWND(hwnd as _), Some(subclass), HOOK_ID, 0).as_bool() {
                log::warn!("taskbar: no hook on the window, so no thumbnail buttons");
            }
        }
    }

    unsafe extern "system" fn subclass(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _: usize, _: usize) -> LRESULT {
        use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, THBN_CLICKED};
        use windows::Win32::UI::WindowsAndMessaging::{WM_COMMAND, WM_NCDESTROY, WM_SETTINGCHANGE};
        if msg == WM_NCDESTROY {
            // SAFETY: the window's last message; the hook goes before the window does.
            unsafe {
                let _ = RemoveWindowSubclass(hwnd, Some(subclass), HOOK_ID);
            }
        } else if let Some(h) = HOOK.get() {
            // Only sent on: what they ask for is done on the app's next turn, never in here.
            let send = |e| {
                let _ = h.events.try_send(e);
            };
            if msg == h.created {
                THUMBS_ADDED.set(false);
                send(Event::Taskbar);
            } else if msg == WM_COMMAND && (wparam.0 >> 16) & 0xFFFF == THBN_CLICKED as usize {
                let event = match (wparam.0 & 0xFFFF) as u32 {
                    THUMB_MUTE => Some(Event::Mute),
                    THUMB_DEAFEN => Some(Event::Deafen),
                    THUMB_LEAVE => Some(Event::Leave),
                    _ => None,
                };
                if let Some(e) = event {
                    send(e);
                    return LRESULT(0);
                }
            } else if msg == WM_SETTINGCHANGE && lparam.0 != 0 {
                // SAFETY: with this message, a non-null lParam is the setting's nul-terminated name.
                if unsafe { PCWSTR(lparam.0 as *const u16).to_string() }.is_ok_and(|s| s == "ImmersiveColorSet") {
                    send(Event::Taskbar);
                }
            }
        }
        // SAFETY: passes the message on to GPUI's own procedure, unchanged.
        unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
    }

    /// Whether the taskbar is dark, which Windows can set apart from the apps' own theme.
    pub fn dark_taskbar() -> bool {
        use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
        let (key, value) = (wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"), wide("SystemUsesLightTheme"));
        let (mut light, mut len) = (0u32, size_of::<u32>() as u32);
        // SAFETY: `light` takes the DWORD, of the size `len` says.
        let ok = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&raw mut light).cast(),
                &mut len,
            )
        };
        ok != 0 || light == 0
    }

    /// The next `set_thumbs` adds the buttons rather than changing them: the window is shown
    /// again, on a new taskbar button.
    pub fn forget_thumbs() {
        THUMBS_ADDED.set(false);
    }

    fn button_icon(name: &'static str, color: &'static str) -> HICON {
        let icon = BUTTON_ICONS.with(|icons| {
            *icons.borrow_mut().entry((name, color)).or_insert_with(|| {
                let size = small_icon();
                pictures::paint(&pictures::button_svg(name, color), size as u32).map_or(0, |px| make_icon(&px, size))
            })
        });
        HICON(icon as _)
    }

    /// Puts the thumbnail's buttons on as `thumbs` says, or hides them outside a call (a taskbar
    /// button keeps the buttons it was given). On the main thread, outside any update of the app:
    /// the taskbar may call back into the window while this waits. False when it did not take
    /// them, before the window has its taskbar button say, so the caller tries again.
    pub fn set_thumbs(hwnd: isize, thumbs: Option<Thumbs>) -> bool {
        use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
        use windows::Win32::UI::Shell::{
            ITaskbarList3, THB_FLAGS, THB_ICON, THB_TOOLTIP, THBF_ENABLED, THBF_HIDDEN, THUMBBUTTON, TaskbarList,
        };
        use windows::Win32::UI::WindowsAndMessaging::IsWindowVisible;
        let added = THUMBS_ADDED.get();
        if thumbs.is_none() && !added {
            return true;
        }
        let hwnd = HWND(hwnd as _);
        // SAFETY: a plain query. Hidden in the tray, the window has no taskbar button; showing it
        // makes a new one, which says so.
        if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
            THUMBS_ADDED.set(false);
            return false;
        }
        let list: Option<ITaskbarList3> = TASKBAR.with(|t| {
            let mut t = t.borrow_mut();
            if t.is_none() {
                // SAFETY: COM is initialised on this thread (GPUI's OleInitialize).
                *t = unsafe { CoCreateInstance::<_, ITaskbarList3>(&TaskbarList, None, CLSCTX_INPROC_SERVER) }
                    .ok()
                    .filter(|l| unsafe { l.HrInit() }.is_ok());
            }
            t.clone()
        });
        let Some(list) = list else { return false };
        let on = thumbs.unwrap_or(Thumbs { muted: false, deafened: false, dark: true });
        let tips = [
            if on.muted { tr!("Unmute", "Ativar microfone") } else { tr!("Mute", "Silenciar") },
            if on.deafened { tr!("Undeafen", "Ativar o som") } else { tr!("Deafen", "Desativar o som") },
            tr!("Disconnect", "Desconectar"),
        ];
        let ids = [THUMB_MUTE, THUMB_DEAFEN, THUMB_LEAVE];
        let mut buttons = [THUMBBUTTON::default(); 3];
        for (b, (((name, color), tip), id)) in buttons.iter_mut().zip(on.pictures().into_iter().zip(tips).zip(ids)) {
            b.dwMask = THB_ICON | THB_TOOLTIP | THB_FLAGS;
            b.iId = id;
            b.hIcon = button_icon(name, color);
            fill(&mut b.szTip, tip);
            b.dwFlags = if thumbs.is_some() { THBF_ENABLED } else { THBF_HIDDEN };
        }
        // SAFETY: `buttons` are complete and outlive the calls. Each way is tried second when the
        // first fails: a new taskbar button not heard of yet, or one that kept its buttons.
        let done = unsafe {
            if added {
                list.ThumbBarUpdateButtons(hwnd, &buttons).or_else(|_| list.ThumbBarAddButtons(hwnd, &buttons))
            } else {
                list.ThumbBarAddButtons(hwnd, &buttons).or_else(|_| list.ThumbBarUpdateButtons(hwnd, &buttons))
            }
        };
        match done {
            Ok(()) => {
                THUMBS_ADDED.set(true);
                true
            }
            Err(e) => {
                log::debug!("taskbar: the thumbnail's buttons were not taken yet ({e})");
                THUMBS_ADDED.set(false);
                false
            }
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::{Event, Look, Menu, Thumbs};
    use std::path::Path;

    pub fn hook_taskbar(_: isize, _: async_channel::Sender<Event>) {}

    pub fn set_thumbs(_: isize, _: Option<Thumbs>) -> bool {
        true
    }

    pub fn forget_thumbs() {}

    pub fn dark_taskbar() -> bool {
        true
    }

    /// One Ciranda per settings folder, as on Windows, through a socket in that folder: a start
    /// that can connect to it finds a copy running, asks it to show its window and exits.
    pub struct Instance {
        listener: Option<std::os::unix::net::UnixListener>,
    }

    const SOCKET: &str = "instance.sock";

    /// Says `what` to the copy listening on `dir`; false when none is.
    fn tell(dir: &Path, what: &[u8]) -> bool {
        use std::io::Write;
        std::os::unix::net::UnixStream::connect(dir.join(SOCKET)).and_then(|mut s| s.write_all(what)).is_ok()
    }

    impl Instance {
        /// `None` when another Ciranda runs on this settings folder: it has been asked to show its
        /// window and this one should exit. After an update restarts Ciranda (`wait`), the copy
        /// being replaced is given time to finish quitting first.
        pub fn claim(data_dir: &Path, wait: bool) -> Option<Instance> {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(if wait { 15 } else { 0 });
            loop {
                match std::os::unix::net::UnixStream::connect(data_dir.join(SOCKET)) {
                    Ok(_) if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(200)),
                    Ok(_) => return (!tell(data_dir, b"open")).then(|| Instance::listen(data_dir)),
                    Err(_) => return Some(Instance::listen(data_dir)),
                }
            }
        }

        /// Harmony never ran here: there is nothing to wait for.
        pub fn claim_legacy(_: &Path, _: bool) -> Option<Instance> {
            Some(Instance { listener: None })
        }

        /// Listens on the folder's socket, replacing one left by a copy that did not exit cleanly.
        fn listen(data_dir: &Path) -> Instance {
            let path = data_dir.join(SOCKET);
            let _ = std::fs::create_dir_all(data_dir);
            let _ = std::fs::remove_file(&path);
            let listener = std::os::unix::net::UnixListener::bind(&path)
                .inspect_err(|e| log::warn!("single instance: {}: {e}, running anyway", path.display()))
                .ok();
            Instance { listener }
        }

        /// Sends `Event::Open` each time another start asks for the window, and `Event::Quit`
        /// when `--quit` asks Ciranda to go.
        pub fn on_show(&self, events: async_channel::Sender<Event>) {
            let Some(listener) = self.listener.as_ref().and_then(|l| l.try_clone().ok()) else { return };
            let spawned = std::thread::Builder::new().name("ciranda-instance".into()).spawn(move || {
                use std::io::Read;
                for stream in listener.incoming() {
                    let mut what = Vec::new();
                    // A start that is only waiting for this copy to go connects and says nothing.
                    let Ok(_) = stream.and_then(|s| s.take(16).read_to_end(&mut what)) else { continue };
                    if what.is_empty() {
                        continue;
                    }
                    if events.send_blocking(if what == b"quit" { Event::Quit } else { Event::Open }).is_err() {
                        break;
                    }
                }
            });
            if spawned.is_err() {
                log::warn!("single instance: no thread to hear other starts");
            }
        }
    }

    /// Asks the Ciranda running on this settings folder to quit, and waits up to `timeout` for it
    /// to be gone; false when it is still there.
    pub fn quit_running(data_dir: &Path, timeout: std::time::Duration) -> bool {
        if !tell(data_dir, b"quit") {
            return true;
        }
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if std::os::unix::net::UnixStream::connect(data_dir.join(SOCKET)).is_err() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        false
    }

    pub struct Tray;

    impl Tray {
        pub fn start(_: async_channel::Sender<Event>) -> Option<Tray> {
            None
        }

        pub fn set_tip(&self, _: &str) {}

        pub fn set_look(&self, _: Look) {}

        pub fn menu(&self, _: Menu) {}

        pub fn notify(&self, _: &str, _: &str) {}

        pub fn remove(&self) {}
    }

    pub fn hide_window(_: isize) {}

    pub fn show_window(_: isize) {}

    pub fn flash_window(_: isize) {}

    pub fn is_minimized(_: isize) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_icon_says_the_most_that_is_true() {
        let call = |muted, deafened, speaking| Some(InCall { muted, deafened, speaking });
        assert_eq!(Look::of(None), Look::Idle);
        assert_eq!(Look::of(call(false, false, false)), Look::Call);
        assert_eq!(Look::of(call(false, false, true)), Look::Speaking);
        // Muted, the mic level reads nothing; if it did, the badge still wins.
        assert_eq!(Look::of(call(true, false, true)), Look::Muted);
        assert_eq!(Look::of(call(true, true, false)), Look::Deafened);
        assert_eq!(Look::of(call(false, true, false)), Look::Deafened);
    }

    #[test]
    fn a_button_that_is_on_is_red_on_either_taskbar() {
        let t = |muted, deafened, dark| Thumbs { muted, deafened, dark }.pictures();
        assert_eq!(
            t(false, false, true),
            [("mic", pictures::INK_DARK), ("headphones", pictures::INK_DARK), ("phone-off", pictures::INK_DARK)]
        );
        assert_eq!(t(true, false, false)[0], ("mic-off", pictures::RED));
        assert_eq!(t(true, true, true)[1], ("headphones-off", pictures::RED));
        assert_eq!(t(false, false, false)[2], ("phone-off", pictures::INK_LIGHT));
    }

    /// The pixel at `(x, y)` of a `size` square picture, as red, green, blue and alpha.
    fn pixel(px: &[u8], size: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * size + x) * 4) as usize;
        [px[i + 2], px[i + 1], px[i], px[i + 3]]
    }

    fn near(a: [u8; 4], hex: &str) -> bool {
        let want = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap().to_be_bytes();
        a[3] > 200 && (0..3).all(|i| a[i].abs_diff(want[i + 1]) <= 12)
    }

    #[test]
    fn the_icons_are_drawn_in_their_colours_at_tray_sizes() {
        assert!(pictures::tray_svg(Look::Idle).is_none(), "Ciranda's own icon outside a call");
        for size in [16, 20, 24, 32] {
            let draw = |look| pictures::paint(&pictures::tray_svg(look).unwrap(), size).unwrap();
            let (call, speaking, muted) = (draw(Look::Call), draw(Look::Speaking), draw(Look::Muted));
            assert_eq!(call.len(), (size * size * 4) as usize);
            // Above the bars, inside the tile.
            let (x, y) = (size / 2, size / 8);
            assert!(near(pixel(&call, size, x, y), pictures::GREY), "{size}: {:?}", pixel(&call, size, x, y));
            assert!(near(pixel(&speaking, size, x, y), pictures::GREEN), "{size}: {:?}", pixel(&speaking, size, x, y));
            // The badge's edge, low on the right, and its gap from the tile.
            let edge = size * 61 / 64;
            assert!(
                near(pixel(&muted, size, edge, size * 43 / 64), pictures::RED),
                "{size}: {:?}",
                pixel(&muted, size, edge, size * 43 / 64)
            );
            assert!(near(pixel(&call, size, edge, size * 43 / 64), pictures::GREY));
            assert_ne!(draw(Look::Muted), draw(Look::Deafened), "the badge shows which");
            // The corners are rounded off, not part of the tile.
            assert_eq!(pixel(&call, size, 0, 0)[3], 0);
        }
    }

    #[test]
    fn button_pictures_are_the_icon_alone() {
        let px = pictures::paint(&pictures::button_svg("mic", pictures::RED), 16).unwrap();
        let alpha: Vec<u8> = px.chunks(4).map(|p| p[3]).collect();
        assert_eq!(alpha[0], 0, "no background");
        let inked: Vec<[u8; 4]> = (0..16 * 16).map(|i| pixel(&px, 16, i % 16, i / 16)).filter(|p| p[3] > 230).collect();
        assert!(inked.len() > 10 && inked.iter().all(|&p| near(p, pictures::RED)), "{}", inked.len());
    }
}
