//! Global hotkeys: key combinations that work while another program, usually a game, has focus,
//! which is exactly when someone wants a mute key. Windows hands a registered combination to us
//! and to nobody else, so a plain letter is refused (it could no longer be typed anywhere), and a
//! combination another program already owns is reported rather than left silently dead.
//!
//! Combinations are kept as Electron accelerators ("Ctrl+Alt+M"), as the old client stored them,
//! and registered with `RegisterHotKey` on a thread of their own, whose message loop also gets
//! the presses. On macOS and Linux `global-hotkey` registers them: Carbon's RegisterEventHotKey on
//! the Mac (no Accessibility permission needed), XGrabKey on Linux, which takes Ciranda running on
//! X11 (XWayland on a Wayland desktop, its default) rather than on Wayland itself.

use crate::core::settings::HotkeyAction;

/// A key with its modifiers. `vk` is the Windows virtual-key code, whatever the platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Combo {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub vk: u16,
}

/// Keys with names of their own: (virtual key, accelerator name).
const NAMED: [(u16, &str); 39] = [
    (0x20, "Space"),
    (0x09, "Tab"),
    (0x0D, "Enter"),
    (0x08, "Backspace"),
    (0x2E, "Delete"),
    (0x2D, "Insert"),
    (0x24, "Home"),
    (0x23, "End"),
    (0x21, "PageUp"),
    (0x22, "PageDown"),
    (0x26, "Up"),
    (0x28, "Down"),
    (0x25, "Left"),
    (0x27, "Right"),
    (0x91, "Scrolllock"),
    (0x2C, "PrintScreen"),
    (0x6B, "numadd"),
    (0x6D, "numsub"),
    (0x6A, "nummult"),
    (0x6F, "numdiv"),
    (0x6E, "numdec"),
    (0xBD, "-"),
    (0xBB, "="),
    (0xDB, "["),
    (0xDD, "]"),
    (0xDC, "\\"),
    (0xBA, ";"),
    (0xDE, "'"),
    (0xBC, ","),
    (0xBE, "."),
    (0xBF, "/"),
    (0xC0, "`"),
    (0xB3, "MediaPlayPause"),
    (0xB0, "MediaNextTrack"),
    (0xB1, "MediaPreviousTrack"),
    (0xB2, "MediaStop"),
    (0xAD, "VolumeMute"),
    (0xAF, "VolumeUp"),
    (0xAE, "VolumeDown"),
];

const F1: u16 = 0x70;
const NUMPAD0: u16 = 0x60;

fn key_name(vk: u16) -> Option<String> {
    Some(match vk {
        0x41..=0x5A | 0x30..=0x39 => (vk as u8 as char).to_string(),
        F1..=0x87 => format!("F{}", vk - F1 + 1),
        NUMPAD0..=0x69 => format!("num{}", vk - NUMPAD0),
        _ => NAMED.iter().find(|(v, _)| *v == vk)?.1.into(),
    })
}

fn key_vk(name: &str) -> Option<u16> {
    let lower = name.to_ascii_lowercase();
    if let [c] = lower.as_bytes()
        && c.is_ascii_alphanumeric()
    {
        return Some(c.to_ascii_uppercase() as u16);
    }
    if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u16>().ok()) {
        return (1..=24).contains(&n).then_some(F1 + n - 1);
    }
    if let Some(n) = lower.strip_prefix("num").and_then(|n| n.parse::<u16>().ok()) {
        return (n <= 9).then_some(NUMPAD0 + n);
    }
    match lower.as_str() {
        "return" => Some(0x0D),
        "esc" | "escape" => None,
        _ => NAMED.iter().find(|(_, n)| n.eq_ignore_ascii_case(name)).map(|(v, _)| *v),
    }
}

/// Why a combination can't be bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A key no hotkey can be made of.
    Unusable,
    /// A key people type with, with no Ctrl, Alt or Win.
    NeedsModifier,
}

impl Refusal {
    pub fn message(self) -> &'static str {
        match self {
            Refusal::Unusable => tr!("That key can't be used as a hotkey.", "Essa tecla não pode ser usada como atalho."),
            Refusal::NeedsModifier if cfg!(target_os = "macos") => tr!(
                "Add Cmd, Option or Ctrl. On its own this key would stop working in every other app while Ciranda is open.",
                "Adicione Cmd, Option ou Ctrl. Sozinha, essa tecla pararia de funcionar em todos os outros apps enquanto Ciranda estiver rodando."
            ),
            Refusal::NeedsModifier => tr!(
                "Add Ctrl or Alt. On its own this key would stop working in every other program while Ciranda is open.",
                "Adicione Ctrl ou Alt. Sozinha, essa tecla pararia de funcionar em todos os outros programas enquanto Ciranda estiver rodando."
            ),
        }
    }
}

impl Combo {
    pub fn parse(accelerator: &str) -> Option<Combo> {
        let mut c = Combo { ctrl: false, alt: false, shift: false, win: false, vk: 0 };
        for part in accelerator.split('+').map(str::trim) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "cmdorctrl" | "commandorcontrol" => c.ctrl = true,
                "alt" | "option" => c.alt = true,
                "shift" => c.shift = true,
                "super" | "meta" | "win" | "cmd" | "command" => c.win = true,
                _ if c.vk == 0 => c.vk = key_vk(part)?,
                _ => return None,
            }
        }
        (c.vk != 0).then_some(c)
    }

    /// As the settings file keeps it: "Ctrl+Alt+Shift+Super+M", modifiers in that order.
    pub fn accelerator(&self) -> String {
        let mut parts: Vec<String> = self.held(["Ctrl", "Alt", "Shift", "Super"]).into_iter().map(String::from).collect();
        parts.push(key_name(self.vk).unwrap_or_default());
        parts.join("+")
    }

    /// The modifiers held, as this system's keyboards name them.
    pub fn modifiers(&self) -> Vec<&'static str> {
        self.held(if cfg!(target_os = "macos") {
            ["Ctrl", "Option", "Shift", "Cmd"]
        } else if cfg!(windows) {
            ["Ctrl", "Alt", "Shift", "Win"]
        } else {
            ["Ctrl", "Alt", "Shift", "Super"]
        })
    }

    fn held(&self, names: [&'static str; 4]) -> Vec<&'static str> {
        [self.ctrl, self.alt, self.shift, self.win].into_iter().zip(names).filter(|m| m.0).map(|m| m.1).collect()
    }

    /// What its keycaps say.
    pub fn keycaps(&self) -> Vec<String> {
        let mut caps: Vec<String> = self.modifiers().into_iter().map(String::from).collect();
        let name = key_name(self.vk).unwrap_or_default();
        caps.push(match self.vk {
            NUMPAD0..=0x69 => format!("Num {}", self.vk - NUMPAD0),
            0x6B => "Num +".into(),
            0x6D => "Num -".into(),
            0x6A => "Num *".into(),
            0x6F => "Num /".into(),
            0x6E => "Num .".into(),
            0x91 => "Scroll Lock".into(),
            0x2C => "Print Screen".into(),
            0xB3 => tr!("Play/Pause", "Tocar/Pausar").into(),
            0xB0 => tr!("Next track", "Próxima faixa").into(),
            0xB1 => tr!("Previous track", "Faixa anterior").into(),
            0xB2 => tr!("Stop", "Parar").into(),
            0xAD => tr!("Mute key", "Tecla de mudo").into(),
            0xAF => "Volume +".into(),
            0xAE => "Volume −".into(),
            _ => name,
        });
        caps
    }

    /// Keys nobody types text with may stand alone (or with Shift): F-keys, the numpad, media and
    /// volume keys, Insert, Scroll Lock and Print Screen. Anything else needs Ctrl, Alt or Win.
    pub fn check(&self) -> Result<(), Refusal> {
        let standalone = matches!(self.vk, F1..=0x87 | NUMPAD0..=0x6F | 0xAD..=0xB3 | 0x2D | 0x91 | 0x2C);
        if self.ctrl || self.alt || self.win || standalone { Ok(()) } else { Err(Refusal::NeedsModifier) }
    }

    /// The combination a key press in the window is, from GPUI's name for the key: on Windows
    /// the virtual key it came from is worked out again, so the numpad is told from the digits
    /// and a shifted symbol counts as Shift and its key.
    pub fn from_keystroke(k: &gpui::Keystroke) -> Result<Combo, Refusal> {
        let m = k.modifiers;
        let mut c = Combo { ctrl: m.control, alt: m.alt, shift: m.shift, win: m.platform, vk: 0 };
        c.vk = match k.key.as_str() {
            "space" => 0x20,
            "tab" => 0x09,
            "enter" => 0x0D,
            "backspace" => 0x08,
            "delete" => 0x2E,
            "insert" => 0x2D,
            "home" => 0x24,
            "end" => 0x23,
            "pageup" => 0x21,
            "pagedown" => 0x22,
            "up" => 0x26,
            "down" => 0x28,
            "left" => 0x25,
            "right" => 0x27,
            key => match key.strip_prefix('f').and_then(|n| n.parse::<u16>().ok()) {
                Some(n) if (1..=24).contains(&n) => F1 + n - 1,
                _ => {
                    let mut chars = key.chars();
                    let (Some(ch), None) = (chars.next(), chars.next()) else { return Err(Refusal::Unusable) };
                    let (vk, shifted) = platform::key_for_char(ch).ok_or(Refusal::Unusable)?;
                    c.shift |= shifted;
                    vk
                }
            },
        };
        key_name(c.vk).ok_or(Refusal::Unusable)?;
        c.check().map(|_| c)
    }
}

/// Why a binding is not registered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// Another program holds it.
    InUse,
    /// Bound to something else as well; the first one has it.
    Duplicate,
    /// Not a combination Windows registers.
    Invalid,
}

impl Failure {
    pub fn message(self) -> &'static str {
        match self {
            Failure::InUse => {
                tr!("Another program already uses this combination. Pick another.", "Outro programa já usa essa combinação. Escolha outra.")
            }
            Failure::Duplicate => tr!("Bound to something else as well.", "Também está ligada a outra coisa."),
            Failure::Invalid if cfg!(windows) => {
                tr!("Not a combination Windows can register.", "Não é uma combinação que o Windows aceita.")
            }
            Failure::Invalid => tr!("Not a combination this system can register.", "Não é uma combinação que este sistema aceita."),
        }
    }
}

/// What the hotkey thread tells the window.
pub enum Event {
    /// The bindings last asked for, each registered or why not.
    Registered(Vec<(HotkeyAction, Option<Failure>)>),
    Pressed(HotkeyAction),
    /// While recording: a combination another program holds was pressed in the window. Windows
    /// would have handed it to that program, so the window never sees it otherwise.
    Taken(Combo),
}

pub use platform::{Registrar, caveat, unavailable};

#[cfg(windows)]
mod platform {
    use super::{Combo, Event, Failure, HotkeyAction};

    /// Why there are no global hotkeys here, for Settings; `None` where there are.
    pub fn unavailable() -> Option<&'static str> {
        None
    }

    /// What to know about them here, for Settings.
    pub fn caveat() -> Option<&'static str> {
        None
    }
    use parking_lot::Mutex;
    use std::collections::HashSet;
    use std::sync::{Arc, OnceLock};
    use windows_sys::Win32::Foundation::{ERROR_HOTKEY_ALREADY_REGISTERED, GetLastError, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, GetKeyState, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey, UnregisterHotKey, VkKeyScanW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, MSG, PM_NOREMOVE,
        PeekMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_APP, WM_HOTKEY, WM_KEYDOWN,
        WM_SYSKEYDOWN,
    };

    /// "Register what `pending` holds now."
    const SET: u32 = WM_APP + 1;
    /// "Recording starts (wParam 1) or stops (0)."
    const RECORD: u32 = WM_APP + 2;
    /// The id a combination is tried under, to learn whether another program holds it.
    const PROBE: i32 = 0xBFFF;

    /// Where the keyboard hook reports, set once with the thread.
    static HOOK_EVENTS: OnceLock<async_channel::Sender<Event>> = OnceLock::new();

    fn mods(c: &Combo) -> u32 {
        [(c.ctrl, MOD_CONTROL), (c.alt, MOD_ALT), (c.shift, MOD_SHIFT), (c.win, MOD_WIN)]
            .into_iter()
            .filter(|m| m.0)
            .fold(MOD_NOREPEAT, |a, m| a | m.1)
    }

    /// While recording, every key pressed in Ciranda's window passes here first. One another
    /// program holds is taken and reported (it would otherwise go to that program and the
    /// recorder would wait forever); everything else goes on to the window.
    unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        // SAFETY: Windows passes a KBDLLHOOKSTRUCT with every HC_ACTION call of this hook.
        unsafe {
            if code == HC_ACTION as i32 && matches!(wparam as u32, WM_KEYDOWN | WM_SYSKEYDOWN) {
                let vk = (*(lparam as *const KBDLLHOOKSTRUCT)).vkCode as u16;
                let modifier = matches!(vk, 0x10..=0x12 | 0xA0..=0xA5 | 0x5B | 0x5C);
                let mut pid = 0;
                GetWindowThreadProcessId(GetForegroundWindow(), &mut pid);
                if !modifier && pid == GetCurrentProcessId() {
                    let down = |k: i32| GetAsyncKeyState(k) < 0;
                    let c = Combo { ctrl: down(0x11), alt: down(0x12), shift: down(0x10), win: down(0x5B) || down(0x5C), vk };
                    if RegisterHotKey(std::ptr::null_mut(), PROBE, mods(&c), vk as u32) != 0 {
                        UnregisterHotKey(std::ptr::null_mut(), PROBE);
                    } else if GetLastError() == ERROR_HOTKEY_ALREADY_REGISTERED
                        && let Some(events) = HOOK_EVENTS.get()
                    {
                        let _ = events.try_send(Event::Taken(c));
                        return 1;
                    }
                }
            }
            CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
        }
    }

    /// The thread that holds the registrations.
    pub struct Registrar {
        thread: u32,
        pending: Arc<Mutex<Vec<(HotkeyAction, Combo)>>>,
    }

    impl Registrar {
        pub fn start(events: async_channel::Sender<Event>) -> Option<Registrar> {
            let pending: Arc<Mutex<Vec<(HotkeyAction, Combo)>>> = Arc::default();
            let (ready, thread) = std::sync::mpsc::channel();
            let p = pending.clone();
            std::thread::Builder::new().name("ciranda-hotkeys".into()).spawn(move || run(p, events, ready)).ok()?;
            Some(Registrar { thread: thread.recv().ok()?, pending })
        }

        /// Replaces every registration with these.
        pub fn set(&self, bindings: Vec<(HotkeyAction, Combo)>) {
            *self.pending.lock() = bindings;
            if unsafe { PostThreadMessageW(self.thread, SET, 0, 0) } == 0 {
                log::warn!("hotkeys: the thread did not take the new bindings");
            }
        }

        /// Watches the window's key presses for combinations other programs hold, while a
        /// recorder is open.
        pub fn record(&self, on: bool) {
            unsafe { PostThreadMessageW(self.thread, RECORD, on as usize, 0) };
        }
    }

    fn run(pending: Arc<Mutex<Vec<(HotkeyAction, Combo)>>>, events: async_channel::Sender<Event>, ready: std::sync::mpsc::Sender<u32>) {
        // SAFETY: plain Win32 calls on this thread's own queue; `msg` outlives every call.
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            // A thread has no message queue until it asks for one, and a post before that is lost.
            PeekMessageW(&mut msg, std::ptr::null_mut(), WM_APP, WM_APP, PM_NOREMOVE);
            let _ = HOOK_EVENTS.set(events.clone());
            let _ = ready.send(GetCurrentThreadId());
            // Registered actions; each one's id is its place here plus one.
            let mut active: Vec<HotkeyAction> = Vec::new();
            let mut hook: HHOOK = std::ptr::null_mut();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                match msg.message {
                    RECORD if msg.wParam == 1 && hook.is_null() => {
                        // A low-level hook runs on the thread that set it, from its message loop.
                        hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), std::ptr::null_mut(), 0);
                        if hook.is_null() {
                            log::warn!("hotkeys: no keyboard hook ({}); taken combinations go unreported", GetLastError());
                        }
                    }
                    RECORD if msg.wParam == 0 && !hook.is_null() => {
                        UnhookWindowsHookEx(hook);
                        hook = std::ptr::null_mut();
                    }
                    WM_HOTKEY => {
                        if let Some(action) = msg.wParam.checked_sub(1).and_then(|i| active.get(i)) {
                            let _ = events.try_send(Event::Pressed(*action));
                        }
                    }
                    SET => {
                        for id in 1..=active.len() {
                            UnregisterHotKey(std::ptr::null_mut(), id as i32);
                        }
                        active.clear();
                        let mut seen = HashSet::new();
                        let mut results = Vec::new();
                        for (action, c) in pending.lock().clone() {
                            let failure = if !seen.insert(c) {
                                Some(Failure::Duplicate)
                            } else if RegisterHotKey(std::ptr::null_mut(), active.len() as i32 + 1, mods(&c), c.vk as u32) != 0 {
                                active.push(action);
                                None
                            } else if GetLastError() == ERROR_HOTKEY_ALREADY_REGISTERED {
                                Some(Failure::InUse)
                            } else {
                                Some(Failure::Invalid)
                            };
                            results.push((action, failure));
                        }
                        let _ = events.try_send(Event::Registered(results));
                    }
                    _ => {}
                }
            }
        }
    }

    /// The key a character was typed with, and whether it took Shift. A digit or operator whose
    /// numpad key is down right now is that numpad key: GPUI names both the same.
    pub fn key_for_char(ch: char) -> Option<(u16, bool)> {
        let numpad = match ch {
            '0'..='9' => Some(super::NUMPAD0 + ch as u16 - '0' as u16),
            '*' => Some(0x6A),
            '+' => Some(0x6B),
            '-' => Some(0x6D),
            '.' | ',' => Some(0x6E),
            '/' => Some(0x6F),
            _ => None,
        };
        // SAFETY: reads this thread's keyboard state, which is the key event being handled.
        if let Some(vk) = numpad.filter(|vk| unsafe { GetKeyState(*vk as i32) } < 0) {
            return Some((vk, false));
        }
        let mut units = [0u16; 2];
        let [unit] = ch.encode_utf16(&mut units) else { return None };
        // SAFETY: a pure lookup in the current keyboard layout.
        let scan = unsafe { VkKeyScanW(*unit) };
        (scan != -1).then_some(((scan & 0xFF) as u16, scan & 0x100 != 0))
    }
}

/// The key codes `global-hotkey` takes on macOS and Linux, for a combination; tested everywhere.
#[cfg(any(target_os = "macos", target_os = "linux", test))]
mod keys {
    use super::{Combo, F1, NUMPAD0};
    use keyboard_types::{Code, Modifiers};

    #[rustfmt::skip]
    const LETTERS: [Code; 26] = [
        Code::KeyA, Code::KeyB, Code::KeyC, Code::KeyD, Code::KeyE, Code::KeyF, Code::KeyG, Code::KeyH, Code::KeyI,
        Code::KeyJ, Code::KeyK, Code::KeyL, Code::KeyM, Code::KeyN, Code::KeyO, Code::KeyP, Code::KeyQ, Code::KeyR,
        Code::KeyS, Code::KeyT, Code::KeyU, Code::KeyV, Code::KeyW, Code::KeyX, Code::KeyY, Code::KeyZ,
    ];
    #[rustfmt::skip]
    const DIGITS: [Code; 10] = [
        Code::Digit0, Code::Digit1, Code::Digit2, Code::Digit3, Code::Digit4,
        Code::Digit5, Code::Digit6, Code::Digit7, Code::Digit8, Code::Digit9,
    ];
    #[rustfmt::skip]
    const NUMPAD: [Code; 10] = [
        Code::Numpad0, Code::Numpad1, Code::Numpad2, Code::Numpad3, Code::Numpad4,
        Code::Numpad5, Code::Numpad6, Code::Numpad7, Code::Numpad8, Code::Numpad9,
    ];
    #[rustfmt::skip]
    const FKEYS: [Code; 24] = [
        Code::F1, Code::F2, Code::F3, Code::F4, Code::F5, Code::F6, Code::F7, Code::F8, Code::F9, Code::F10, Code::F11, Code::F12,
        Code::F13, Code::F14, Code::F15, Code::F16, Code::F17, Code::F18, Code::F19, Code::F20, Code::F21, Code::F22, Code::F23, Code::F24,
    ];

    /// The key for a virtual-key code, on the Mac (`mac`) or under X11. The Mac's hotkeys stop at
    /// F20, and its media and volume keys reach a program only through an event tap, which needs
    /// the Accessibility permission: those are not offered there. X11 knows the play/pause key as
    /// "play".
    pub fn code(vk: u16, mac: bool) -> Option<Code> {
        Some(match vk {
            0x41..=0x5A => LETTERS[(vk - 0x41) as usize],
            0x30..=0x39 => DIGITS[(vk - 0x30) as usize],
            F1..=0x83 if mac => FKEYS[(vk - F1) as usize],
            F1..=0x87 if !mac => FKEYS[(vk - F1) as usize],
            NUMPAD0..=0x69 => NUMPAD[(vk - NUMPAD0) as usize],
            0xAD..=0xB3 if mac => return None,
            0x20 => Code::Space,
            0x09 => Code::Tab,
            0x0D => Code::Enter,
            0x08 => Code::Backspace,
            0x2E => Code::Delete,
            0x2D => Code::Insert,
            0x24 => Code::Home,
            0x23 => Code::End,
            0x21 => Code::PageUp,
            0x22 => Code::PageDown,
            0x26 => Code::ArrowUp,
            0x28 => Code::ArrowDown,
            0x25 => Code::ArrowLeft,
            0x27 => Code::ArrowRight,
            0x91 => Code::ScrollLock,
            0x2C => Code::PrintScreen,
            0x6B => Code::NumpadAdd,
            0x6D => Code::NumpadSubtract,
            0x6A => Code::NumpadMultiply,
            0x6F => Code::NumpadDivide,
            0x6E => Code::NumpadDecimal,
            0xBD => Code::Minus,
            0xBB => Code::Equal,
            0xDB => Code::BracketLeft,
            0xDD => Code::BracketRight,
            0xDC => Code::Backslash,
            0xBA => Code::Semicolon,
            0xDE => Code::Quote,
            0xBC => Code::Comma,
            0xBE => Code::Period,
            0xBF => Code::Slash,
            0xC0 => Code::Backquote,
            0xB3 => Code::MediaPlay,
            0xB0 => Code::MediaTrackNext,
            0xB1 => Code::MediaTrackPrevious,
            0xB2 => Code::MediaStop,
            0xAD => Code::AudioVolumeMute,
            0xAF => Code::AudioVolumeUp,
            0xAE => Code::AudioVolumeDown,
            _ => return None,
        })
    }

    /// Win is the Mac's Cmd and Linux's Super.
    pub fn modifiers(c: &Combo) -> Modifiers {
        [(c.ctrl, Modifiers::CONTROL), (c.alt, Modifiers::ALT), (c.shift, Modifiers::SHIFT), (c.win, Modifiers::META)]
            .into_iter()
            .filter(|m| m.0)
            .fold(Modifiers::empty(), |a, m| a | m.1)
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub fn hotkey(c: &Combo) -> Option<global_hotkey::hotkey::HotKey> {
        Some(global_hotkey::hotkey::HotKey::new(Some(modifiers(c)), code(c.vk, cfg!(target_os = "macos"))?))
    }
}

/// `global-hotkey`. On macOS its handler sits on the application's event target and the presses
/// come through the run loop GPUI runs, so it is made and changed on the main thread, where it is
/// quick. On Linux each change is a round trip to its X11 thread, which looks for work every 50
/// ms: changes go through a thread of their own so the window never waits on them.
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod platform {
    use super::{Combo, Event, Failure, HotkeyAction, keys};
    use global_hotkey::hotkey::HotKey;
    use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
    use parking_lot::Mutex;
    use std::collections::{HashMap, HashSet};
    use std::sync::OnceLock;

    /// Where presses go: the window's events, and the action each registered combination does.
    struct Route {
        events: async_channel::Sender<Event>,
        actions: Mutex<HashMap<u32, HotkeyAction>>,
    }

    static ROUTE: OnceLock<Route> = OnceLock::new();

    /// Ciranda runs on X11 (XWayland included), where XGrabKey works. On Wayland itself nothing
    /// can listen for keys meant for other windows.
    #[cfg(target_os = "linux")]
    fn on_x11() -> bool {
        let set = |var: &str| std::env::var_os(var).is_some_and(|v| !v.is_empty());
        set("DISPLAY") && !set("WAYLAND_DISPLAY") && std::env::var_os("CIRANDA_WAYLAND").is_none()
    }

    pub fn unavailable() -> Option<&'static str> {
        #[cfg(target_os = "linux")]
        if !on_x11() {
            return Some(tr!(
                "Global hotkeys need X11. Ciranda is running on Wayland here (CIRANDA_WAYLAND is set, or there is no X server), where no program can listen for keys pressed in other windows. Start it without CIRANDA_WAYLAND to use them.",
                "Atalhos globais precisam do X11. Aqui Ciranda está rodando no Wayland (CIRANDA_WAYLAND está definida, ou não há servidor X), onde nenhum programa pode ouvir teclas pressionadas em outras janelas. Abra sem CIRANDA_WAYLAND para usá-los."
            ));
        }
        None
    }

    /// On a Wayland desktop, XWayland hands X11 programs the keys only while one of them has
    /// focus: most games do, a browser or a native Wayland app does not.
    pub fn caveat() -> Option<&'static str> {
        #[cfg(target_os = "linux")]
        if std::env::var("XDG_SESSION_TYPE").is_ok_and(|s| s.eq_ignore_ascii_case("wayland")) {
            return Some(tr!(
                "On this Wayland desktop they work while an X11 window has focus, as most games are, but not over native Wayland apps.",
                "Neste desktop Wayland eles funcionam enquanto uma janela X11 está em foco, como a maioria dos jogos, mas não sobre apps Wayland nativos."
            ));
        }
        None
    }

    pub struct Registrar {
        #[cfg(target_os = "macos")]
        manager: GlobalHotKeyManager,
        #[cfg(target_os = "macos")]
        registered: Mutex<Vec<HotKey>>,
        #[cfg(target_os = "linux")]
        work: std::sync::mpsc::Sender<Vec<(HotkeyAction, Combo)>>,
    }

    impl Registrar {
        /// On the main thread.
        pub fn start(events: async_channel::Sender<Event>) -> Option<Registrar> {
            if let Some(why) = unavailable() {
                log::info!("hotkeys: {why}");
                return None;
            }
            ROUTE.set(Route { events, actions: Mutex::default() }).ok()?;
            GlobalHotKeyEvent::set_event_handler(Some(|e: GlobalHotKeyEvent| {
                if e.state != HotKeyState::Pressed {
                    return;
                }
                let Some(route) = ROUTE.get() else { return };
                if let Some(action) = route.actions.lock().get(&e.id).copied() {
                    let _ = route.events.try_send(Event::Pressed(action));
                }
            }));
            #[cfg(target_os = "macos")]
            {
                let manager = GlobalHotKeyManager::new().inspect_err(|e| log::warn!("hotkeys: {e}")).ok()?;
                Some(Registrar { manager, registered: Mutex::default() })
            }
            #[cfg(target_os = "linux")]
            {
                let manager = GlobalHotKeyManager::new().inspect_err(|e| log::warn!("hotkeys: {e}")).ok()?;
                let (work, todo) = std::sync::mpsc::channel::<Vec<(HotkeyAction, Combo)>>();
                std::thread::Builder::new()
                    .name("ciranda-hotkeys".into())
                    .spawn(move || {
                        let mut registered = Vec::new();
                        while let Ok(mut bindings) = todo.recv() {
                            // Only the newest set matters.
                            while let Ok(newer) = todo.try_recv() {
                                bindings = newer;
                            }
                            apply(&manager, &mut registered, bindings);
                        }
                    })
                    .inspect_err(|e| log::warn!("hotkeys: no thread for them: {e}"))
                    .ok()?;
                Some(Registrar { work })
            }
        }

        /// Replaces every registration with these.
        pub fn set(&self, bindings: Vec<(HotkeyAction, Combo)>) {
            #[cfg(target_os = "macos")]
            apply(&self.manager, &mut self.registered.lock(), bindings);
            #[cfg(target_os = "linux")]
            if self.work.send(bindings).is_err() {
                log::warn!("hotkeys: the thread did not take the new bindings");
            }
        }

        /// Windows alone can tell a combination another program holds while recording.
        pub fn record(&self, _: bool) {}
    }

    fn apply(manager: &GlobalHotKeyManager, registered: &mut Vec<HotKey>, bindings: Vec<(HotkeyAction, Combo)>) {
        let Some(route) = ROUTE.get() else { return };
        for hotkey in registered.drain(..) {
            if let Err(e) = manager.unregister(hotkey) {
                log::warn!("hotkeys: {e}");
            }
        }
        let (mut actions, mut seen, mut results) = (HashMap::new(), HashSet::new(), Vec::new());
        for (action, c) in bindings {
            let failure = if !seen.insert(c) {
                Some(Failure::Duplicate)
            } else if let Some(hotkey) = keys::hotkey(&c) {
                match manager.register(hotkey) {
                    Ok(()) => {
                        registered.push(hotkey);
                        actions.insert(hotkey.id(), action);
                        None
                    }
                    Err(global_hotkey::Error::AlreadyRegistered(_)) => Some(Failure::InUse),
                    Err(e) => {
                        log::info!("hotkeys: {} not registered: {e}", c.accelerator());
                        Some(Failure::Invalid)
                    }
                }
            } else {
                Some(Failure::Invalid)
            };
            results.push((action, failure));
        }
        *route.actions.lock() = actions;
        let _ = route.events.try_send(Event::Registered(results));
    }

    pub fn key_for_char(ch: char) -> Option<(u16, bool)> {
        ch.is_ascii_alphanumeric().then(|| (ch.to_ascii_uppercase() as u16, false))
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod platform {
    use super::{Combo, Event, HotkeyAction};

    pub fn unavailable() -> Option<&'static str> {
        Some(tr!("Global hotkeys aren't available on this system.", "Atalhos globais não estão disponíveis neste sistema."))
    }

    pub fn caveat() -> Option<&'static str> {
        None
    }

    pub struct Registrar;

    impl Registrar {
        pub fn start(_: async_channel::Sender<Event>) -> Option<Registrar> {
            None
        }

        pub fn set(&self, _: Vec<(HotkeyAction, Combo)>) {}

        pub fn record(&self, _: bool) {}
    }

    pub fn key_for_char(ch: char) -> Option<(u16, bool)> {
        ch.is_ascii_alphanumeric().then(|| (ch.to_ascii_uppercase() as u16, false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accelerators_round_trip_as_the_old_client_wrote_them() {
        for s in
            ["Ctrl+Shift+M", "Ctrl+Alt+M", "F7", "Shift+F13", "num5", "Alt+numadd", "Super+=", "Ctrl+Alt+`", "MediaPlayPause", "Ctrl+Space"]
        {
            let c = Combo::parse(s).unwrap_or_else(|| panic!("{s}"));
            assert_eq!(c.accelerator(), s);
        }
        assert_eq!(Combo::parse("control+shift+m").map(|c| c.accelerator()).as_deref(), Some("Ctrl+Shift+M"));
        assert_eq!(Combo::parse("Ctrl+Alt+M").map(|c| c.vk), Some(0x4D));
        assert_eq!(Combo::parse("F24").map(|c| c.vk), Some(0x87));
        for bad in ["", "Ctrl", "Ctrl+M+K", "F25", "num10", "Ctrl+Esc", "Ctrl+Banana"] {
            assert_eq!(Combo::parse(bad), None, "{bad}");
        }
        let alt = if cfg!(target_os = "macos") { "Option" } else { "Alt" };
        assert_eq!(Combo::parse("Ctrl+Alt+numdec").unwrap().keycaps(), ["Ctrl", alt, "Num ."]);
    }

    #[test]
    fn a_key_people_type_with_needs_ctrl_alt_or_win() {
        let check = |s: &str| Combo::parse(s).unwrap().check();
        for ok in ["Ctrl+M", "Alt+1", "Super+K", "F5", "Shift+F5", "num0", "Shift+num7", "MediaNextTrack", "VolumeUp", "Insert"] {
            assert_eq!(check(ok), Ok(()), "{ok}");
        }
        for refused in ["M", "Shift+M", "1", "Space", "Shift+Enter", "-"] {
            assert_eq!(check(refused), Err(Refusal::NeedsModifier), "{refused}");
        }
    }

    fn press(key: &str, ctrl: bool, alt: bool, shift: bool) -> Result<Combo, Refusal> {
        let modifiers = gpui::Modifiers { control: ctrl, alt, shift, ..Default::default() };
        Combo::from_keystroke(&gpui::Keystroke { modifiers, key: key.into(), key_char: None })
    }

    #[test]
    fn every_key_a_binding_can_name_has_a_code_on_linux_and_most_on_the_mac() {
        use keyboard_types::{Code, Modifiers};
        let bindable = (0x41..=0x5A).chain(0x30..=0x39).chain(F1..=0x87).chain(NUMPAD0..=0x69).chain(NAMED.iter().map(|n| n.0));
        for vk in bindable {
            assert!(keys::code(vk, false).is_some(), "{vk:#x} under X11");
            let mac_lacks = matches!(vk, 0x84..=0x87 | 0xAD..=0xB3);
            assert_eq!(keys::code(vk, true).is_none(), mac_lacks, "{vk:#x} on the Mac");
        }
        let code = |s: &str| keys::code(Combo::parse(s).unwrap().vk, false);
        assert_eq!(code("Ctrl+M"), Some(Code::KeyM));
        assert_eq!(code("num7"), Some(Code::Numpad7));
        assert_eq!(code("F24"), Some(Code::F24));
        assert_eq!(code("Alt+`"), Some(Code::Backquote));
        assert_eq!(code("MediaPlayPause"), Some(Code::MediaPlay));
        assert_eq!(keys::code(0x1B, false), None, "Escape is never a hotkey");
        let mods = |s: &str| keys::modifiers(&Combo::parse(s).unwrap());
        assert_eq!(mods("Ctrl+Shift+M"), Modifiers::CONTROL | Modifiers::SHIFT);
        assert_eq!(mods("Alt+Super+K"), Modifiers::ALT | Modifiers::META);
        assert_eq!(mods("F5"), Modifiers::empty());
    }

    #[test]
    fn key_presses_become_combinations() {
        assert_eq!(press("m", true, true, false).map(|c| c.accelerator()), Ok("Ctrl+Alt+M".into()));
        assert_eq!(press("f9", false, false, false).map(|c| c.accelerator()), Ok("F9".into()));
        assert_eq!(press("space", false, true, false).map(|c| c.accelerator()), Ok("Alt+Space".into()));
        assert_eq!(press("m", false, false, false), Err(Refusal::NeedsModifier));
        assert_eq!(press("m", false, false, true), Err(Refusal::NeedsModifier));
        assert_eq!(press("escape", true, false, false), Err(Refusal::Unusable));
    }
}
