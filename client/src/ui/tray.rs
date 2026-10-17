//! Closing the window to the notification area (the icon itself is `crate::tray`), and what the
//! icon and the taskbar's thumbnail ask for: the window back, mute, deafen and disconnect in a
//! call, quitting. In a call both show how you are in it.

use super::Screen;
use super::overlay::RootHandle;
use super::server::ServerView;
use crate::core::settings::HotkeyAction;
use crate::media::call::Call;
use crate::prefs::{prefs, set_prefs};
use crate::tray::{Event, InCall, Instance, Look, Menu, Thumbs, Tray};
use gpui::{AnyWindowHandle, App, Entity, Global, Subscription, WeakEntity, Window};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::time::Duration;

struct TrayState {
    tray: Option<Tray>,
    window: AnyWindowHandle,
    hwnd: Option<isize>,
    /// The tooltip last given to the icon.
    tip: String,
    /// The call in progress, which the icon and the thumbnail follow, and what hears it change.
    call: Option<WeakEntity<Call>>,
    _call_changed: Option<Subscription>,
    /// What the icon was last set to. Talking flips it several times a second, and only a
    /// change reaches the shell.
    look: Look,
    /// What the thumbnail's buttons were last set to (`None` inside: not in a call); `None` when
    /// they still have to be, after a failure or a new taskbar button.
    thumbs: Option<Option<(bool, bool)>>,
    /// The taskbar is dark, as last read.
    dark: bool,
}

impl Global for TrayState {}

pub fn init(window: AnyWindowHandle, instance: &Instance, cx: &mut App) {
    let (tx, rx) = async_channel::unbounded();
    instance.on_show(tx.clone());
    let hwnd = window.update(cx, |_, window, _| hwnd(window)).ok().flatten();
    // Now, before the message loop runs again: the taskbar says when the window's button is
    // there, and the buttons can only go on after that.
    if let Some(hwnd) = hwnd {
        crate::tray::hook_taskbar(hwnd, tx.clone());
    }
    let tray = Tray::start(tx);
    if tray.is_none() {
        log::info!("no notification-area icon here: closing the window quits");
    }
    cx.set_global(TrayState {
        tray,
        window,
        hwnd,
        tip: String::new(),
        call: None,
        _call_changed: None,
        look: Look::Idle,
        thumbs: None,
        dark: crate::tray::dark_taskbar(),
    });
    let _ = window.update(cx, |_, window, cx| window.on_window_should_close(cx, on_close));
    cx.on_app_quit(|cx| {
        if let Some(tray) = &cx.global::<TrayState>().tray {
            tray.remove();
        }
        async {}
    })
    .detach();
    cx.spawn(async move |cx| {
        while let Ok(ev) = rx.recv().await {
            cx.update(|cx| on_event(ev, cx));
        }
    })
    .detach();
}

pub(crate) fn hwnd(window: &Window) -> Option<isize> {
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

/// The call in progress, or none: the icon, its tooltip and the thumbnail's buttons follow it.
pub fn set_call(call: Option<&Entity<Call>>, cx: &mut App) {
    if !cx.has_global::<TrayState>() {
        return;
    }
    let changed = call.map(|c| cx.observe(c, |_, cx| sync(cx)));
    let state = cx.global_mut::<TrayState>();
    state.call = call.map(Entity::downgrade);
    state._call_changed = changed;
    // Called from inside the server view's update, which `tip` reads.
    cx.defer(sync);
}

/// Brings the icon, its tooltip and the thumbnail up to date with the call, each only when what
/// it shows changed.
fn sync(cx: &mut App) {
    let Some(state) = cx.try_global::<TrayState>() else { return };
    let now = state.call.as_ref().and_then(WeakEntity::upgrade).map(|call| {
        let c = call.read(cx);
        let me = c.session.read(cx).me.id;
        InCall { muted: c.muted, deafened: c.deafened, speaking: c.is_speaking(me, cx) }
    });
    let look = Look::of(now);
    let thumbs = now.map(|c| (c.muted, c.deafened));
    let tip = tip(cx);
    let state = cx.global_mut::<TrayState>();
    if let Some(tray) = &state.tray {
        if state.look != look {
            tray.set_look(look);
        }
        if state.tip != tip {
            tray.set_tip(&tip);
        }
    }
    state.look = look;
    state.tip = tip;
    if state.thumbs == Some(thumbs) {
        return;
    }
    state.thumbs = Some(thumbs);
    let (Some(hwnd), dark) = (state.hwnd, state.dark) else { return };
    // On the app's next turn, outside every update: the taskbar may call into the window.
    cx.spawn(async move |cx| {
        let thumbs = thumbs.map(|(muted, deafened)| Thumbs { muted, deafened, dark });
        if !crate::tray::set_thumbs(hwnd, thumbs) {
            // Tried again on the next change, or once the taskbar button says it is there.
            cx.update(|cx| cx.global_mut::<TrayState>().thumbs = None);
        }
    })
    .detach();
}

/// The window's close button. With the icon there and the setting on, the window only hides;
/// otherwise Ciranda quits, keeping the window until then so a call is left properly.
fn on_close(window: &mut Window, cx: &mut App) -> bool {
    let hide = cx.global::<TrayState>().tray.is_some() && prefs(cx).close_to_tray;
    let Some(hwnd) = hwnd(window).filter(|_| hide) else {
        cx.quit();
        return false;
    };
    crate::tray::hide_window(hwnd);
    if !prefs(cx).tray_notice_seen {
        set_prefs(cx, |p| p.tray_notice_seen = true);
        if let Some(tray) = &cx.global::<TrayState>().tray {
            tray.notify(
                tr!("Ciranda is still running in the tray", "Ciranda continua rodando na bandeja"),
                tr!(
                    "Calls and messages keep going. Click the icon to open it again, or right-click it to quit.",
                    "Chamadas e mensagens continuam. Clique no ícone para abrir de novo, ou com o botão direito para sair."
                ),
            );
        }
    }
    false
}

/// Something needs you while the window is in the background: a call, a private message. The
/// taskbar button flashes and, from the tray, Windows shows a notification. Says only who, never
/// what: a notification is on a screen anyone nearby can read.
pub fn attention(window: &Window, title: &str, text: &str, cx: &App) {
    if window.is_window_active() {
        return;
    }
    if let Some(hwnd) = hwnd(window) {
        crate::tray::flash_window(hwnd);
    }
    if let Some(tray) = cx.try_global::<TrayState>().and_then(|s| s.tray.as_ref()) {
        tray.notify(title, text);
    }
}

fn show(cx: &mut App) {
    let window = cx.global::<TrayState>().window;
    let _ = window.update(cx, |_, window, _| {
        if let Some(hwnd) = hwnd(window) {
            crate::tray::show_window(hwnd);
        }
        window.activate_window();
    });
    // Back from the tray on a new taskbar button, which has none of the thumbnail's buttons. It
    // says so when it is made; this is in case it said so before the window could hear it.
    cx.spawn(async move |cx| {
        cx.background_executor().timer(Duration::from_millis(500)).await;
        cx.update(|cx| {
            crate::tray::forget_thumbs();
            cx.global_mut::<TrayState>().thumbs = None;
            sync(cx);
        });
    })
    .detach();
}

fn server_view(cx: &App) -> Option<Entity<ServerView>> {
    let root = cx.try_global::<RootHandle>()?.0.upgrade()?;
    match &root.read(cx).screen {
        Screen::Server(v) => Some(v.clone()),
        Screen::Connect { .. } => None,
    }
}

/// Muted and deafened, in a call.
fn call_state(cx: &App) -> Option<(bool, bool)> {
    let view = server_view(cx)?;
    let call = view.read(cx).call.as_ref()?.read(cx);
    Some((call.muted, call.deafened))
}

/// "Ciranda", then the server and the call, one to a line.
fn tip(cx: &App) -> String {
    let Some(view) = server_view(cx) else { return "Ciranda".into() };
    let view = view.read(cx);
    let session = view.session.read(cx);
    let mut tip = format!("Ciranda\n{}", session.server_name);
    if let Some(call) = &view.call {
        let call = call.read(cx);
        let channel = session.place_name(call.place);
        tip.push('\n');
        tip.push_str(&trf!("In call: {}", "Em chamada: {}", channel));
        if call.deafened {
            tip.push_str(&format!(" · {}", tr!("Deafened", "Som desativado")));
        } else if call.muted {
            tip.push_str(&format!(" · {}", tr!("Muted", "Silenciado")));
        }
    }
    tip
}

fn on_event(ev: Event, cx: &mut App) {
    match ev {
        Event::Open => show(cx),
        Event::Menu => {
            let menu = Menu { call: call_state(cx) };
            if let Some(tray) = &cx.global::<TrayState>().tray {
                tray.menu(menu);
            }
        }
        Event::Hover => {
            let tip = tip(cx);
            let state = cx.global_mut::<TrayState>();
            if state.tip != tip
                && let Some(tray) = &state.tray
            {
                tray.set_tip(&tip);
                state.tip = tip;
            }
        }
        // As the window's own buttons and the hotkeys do, so the cues and an admin's mute apply.
        Event::Mute | Event::Deafen => {
            let action = if matches!(ev, Event::Mute) { HotkeyAction::Mute } else { HotkeyAction::Deafen };
            if let Some(view) = server_view(cx) {
                view.update(cx, |v, cx| v.on_hotkey(action, cx));
            }
        }
        // Only while there is still a call: a click can arrive just after it ended.
        Event::Leave => {
            if let Some(view) = server_view(cx).filter(|v| v.read(cx).call.is_some()) {
                view.update(cx, |v, cx| v.leave_voice(cx));
            }
        }
        Event::Taskbar => {
            let state = cx.global_mut::<TrayState>();
            state.dark = crate::tray::dark_taskbar();
            state.thumbs = None;
            sync(cx);
        }
        Event::Quit => cx.quit(),
    }
}
