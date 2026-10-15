//! The window's root: the sign-in screen or the server, the theme, and what floats over them
//! (dialogs, menus, toasts).

pub mod camera;
pub mod connect;
mod elsewhere;
pub mod hotkeys;
pub mod overlay;
mod rail;
pub mod server;
pub mod settings;
pub mod tray;
pub mod updates;

use crate::prefs::prefs;
use crate::session::{Link, Session, SessionEvent};
use crate::theme::{self, CustomColors, FONT, Theme, px, radius};
use crate::widgets::*;
use connect::{ConnectEvent, ConnectView, Connected};
use gpui::prelude::FluentBuilder;
use gpui::{
    Animation, AnimationExt, AnyElement, App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyDownEvent, ParentElement,
    Render, RenderImage, SharedString, Styled, Subscription, Window, div,
};
use overlay::{Overlay, Toast};
use server::{ServerView, Then};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// How often the background servers are matched against the rail again, so one that could not be
/// reached at first is tried again.
const SYNC_EVERY_TICKS: u32 = 23;
/// The ring's beat, as the server view rings it.
const RING_TICK: Duration = Duration::from_millis(2600);

enum Screen {
    Connect { view: Entity<ConnectView> },
    Server(Entity<ServerView>),
}

pub struct Root {
    focus: FocusHandle,
    screen: Screen,
    pub overlay: Overlay,
    toasts: Vec<Toast>,
    next_toast: usize,
    _subs: Vec<Subscription>,
    /// What the current screen listens to, dropped with it.
    screen_subs: Vec<Subscription>,
    /// The rail's server pictures by hash; `None` while one loads or when it would not.
    rail_icons: HashMap<String, Option<Arc<RenderImage>>>,
    /// What each background session is listened to with, by its entity.
    away_subs: HashMap<gpui::EntityId, Vec<Subscription>>,
    /// A call ringing from a server in the background, and its dialog.
    away_ringing: Option<(gpui::EntityId, Entity<server::private::Incoming>)>,
    /// When a background message last made a sound.
    away_cue_at: Option<std::time::Instant>,
    /// Where `go_to` is going (server, username) and what to do once there.
    pending: Option<(String, String, Then)>,
    /// What to do on the server on screen once its socket is up.
    then: Option<Then>,
}

impl Focusable for Root {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// The theme the settings ask for, on this window.
pub fn theme_for(window: &Window, cx: &App) -> Theme {
    let p = prefs(cx);
    let forced = std::env::var("CIRANDA_THEME").ok();
    let custom = p.custom_theme.as_ref().and_then(CustomColors::from_settings);
    Theme::new(forced.as_deref().unwrap_or(&p.theme), window.appearance(), custom.as_ref())
}

impl Root {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Root {
        // The theme and the interface size can change with either; parts of the window that are
        // cached (the chat, the channel and member lists) would keep the old ones, so everything
        // draws again. A slider's level shows only in what is drawn afresh anyway.
        let mut subs = vec![cx.observe_window_appearance(window, |_, window, _| window.refresh())];
        subs.push(cx.observe_global_in::<crate::prefs::Prefs>(window, |_, window, cx| {
            if !cx.global::<crate::prefs::Prefs>().redraw {
                cx.notify();
                return;
            }
            hotkeys::sync(cx);
            window.refresh();
        }));
        // Minimised or in the tray: video keeps flowing to others, but nothing here is turned
        // into pictures for a window nobody sees.
        subs.push(cx.observe_window_visibility(window, |_, visibility, _, _| {
            crate::media::video::set_window_hidden(visibility == gpui::WindowVisibility::Hidden);
        }));
        // Where the window is, for the next start; a minimised window's place says nothing.
        subs.push(cx.observe_window_bounds(window, |_, window, cx| {
            if tray::hwnd(window).is_some_and(crate::tray::is_minimized) {
                return;
            }
            let (b, maximized) = match window.window_bounds() {
                gpui::WindowBounds::Maximized(b) => (b, true),
                gpui::WindowBounds::Windowed(b) | gpui::WindowBounds::Fullscreen(b) => (b, false),
            };
            let place = crate::core::settings::WindowPlace {
                x: b.origin.x.into(),
                y: b.origin.y.into(),
                width: b.size.width.into(),
                height: b.size.height.into(),
                maximized,
            };
            crate::prefs::set_prefs(cx, |s| s.window = Some(place));
        }));
        let view =cx.new(|cx| ConnectView::new(None, window, cx));
        // The ring of a call from a server in the background, and every minute or so, the
        // background servers matched against the rail again.
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let mut tick = 0u32;
            loop {
                cx.background_executor().timer(RING_TICK).await;
                tick += 1;
                let sync = tick % SYNC_EVERY_TICKS == 0;
                let Ok(()) = handle.update(cx, |_, window, cx| {
                    let _ = this.update(cx, |root: &mut Root, cx| {
                        if root.away_ringing.is_some() {
                            crate::media::audio::cue(crate::media::audio::Cue::Ring, crate::media::voice::cue_volume(cx).max(0.6));
                        }
                        if sync && matches!(root.screen, Screen::Server(_)) {
                            root.sync_elsewhere(window, cx);
                        }
                    });
                }) else {
                    break;
                };
            }
        })
        .detach();
        let mut root = Root {
            focus: cx.focus_handle(),
            screen: Screen::Connect { view: view.clone() },
            overlay: Overlay::default(),
            toasts: Vec::new(),
            next_toast: 0,
            _subs: subs,
            screen_subs: Vec::new(),
            rail_icons: HashMap::new(),
            away_subs: HashMap::new(),
            away_ringing: None,
            away_cue_at: None,
            pending: None,
            then: None,
        };
        root.subscribe_connect(&view, window, cx);
        root
    }

    pub fn toast(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.next_toast += 1;
        let id = self.next_toast;
        self.toasts.push(Toast { id, text: text.into() });
        if self.toasts.len() > 3 {
            self.toasts.remove(0);
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(5000)).await;
            let _ = this.update(cx, |this, cx| {
                this.toasts.retain(|t| t.id != id);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if ev.keystroke.key == "escape" && self.overlay.close_top() {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let _ = window;
    }
}

/// The Ciranda mark: a microphone on a stand inside a rounded square, in the accent, as the app
/// icon draws it.
pub fn brand_mark(size: f32, t: &Theme) -> gpui::Div {
    let bar = |h: f32| div().w(px(size * 0.085)).h(px(size * h)).rounded(px(size * 0.05)).bg(t.on_accent);
    div()
        .flex_none()
        .size(px(size))
        .rounded(px(size * 0.26))
        .bg(t.accent)
        .flex()
        .items_center()
        .justify_center()
        .gap(px(size * 0.055))
        .children([0.18, 0.36, 0.52, 0.36, 0.18].map(bar))
}

/// What shows while a kept sign-in goes straight in: the mark alone, its bars rising and falling
/// once like a voice, then resting a second in their usual shape before the next. It fades in,
/// so a quick sign-in barely shows it.
pub fn loading_screen(caption_text: SharedString, t: &Theme) -> impl IntoElement {
    const MARK: f32 = 72.;
    /// One pulse and the rest after it.
    const CYCLE_MS: f32 = 1700.;
    /// How long each bar takes to rise and fall, and how much later each pair from the middle
    /// starts: the middle bar first, so the pulse spreads outward and the mark stays symmetric.
    const RISE_MS: f32 = 500.;
    const SPREAD_MS: f32 = 100.;
    // The bars' heights in the mark (as `brand_mark` draws them) and at the top of a pulse.
    let rest = [0.18, 0.36, 0.52, 0.36, 0.18];
    let lift = 0.2;
    let mark = div()
        .flex_none()
        .size(px(MARK))
        .rounded(px(MARK * 0.26))
        .bg(t.accent)
        .flex()
        .items_center()
        .justify_center()
        .gap(px(MARK * 0.055))
        .with_animation("loading-mark", Animation::new(Duration::from_millis(CYCLE_MS as u64)).repeat(), {
            let bar_color = t.on_accent;
            move |d, delta| {
                d.children(rest.iter().enumerate().map(|(i, rest)| {
                    let from_middle = (i as f32 - 2.).abs();
                    let x = (delta * CYCLE_MS - from_middle * SPREAD_MS) / RISE_MS;
                    let pulse = if (0. ..=1.).contains(&x) { (std::f32::consts::PI * x).sin().powi(2) } else { 0. };
                    let h = rest + lift * pulse;
                    div().w(px(MARK * 0.085)).h(px(MARK * h)).rounded(px(MARK * 0.05)).bg(bar_color)
                }))
            }
        });
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(20.))
        .bg(t.base)
        .child(mark)
        .child(caption(caption_text, t.text3))
        .with_animation("loading-in", Animation::new(Duration::from_millis(150)).with_easing(gpui::ease_in_out), |d, delta| {
            d.opacity(delta)
        })
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        theme::set_scale(prefs(cx).ui_scale as f32 / 100.);
        let t = theme_for(window, cx);
        theme::set_current(t);
        let screen: AnyElement = match &self.screen {
            Screen::Connect { view, .. } => view.clone().into_any_element(),
            Screen::Server(v) => v.clone().into_any_element(),
        };
        // A kept sign-in going in shows the loading screen alone, without the rail beside it.
        let resuming = matches!(&self.screen, Screen::Connect { view } if view.read(cx).resuming());
        // So is a stream watched over the whole window.
        let theater = matches!(&self.screen, Screen::Server(v) if v.read(cx).theater.is_some());
        let rail = (!resuming && !theater && !prefs(cx).saved_servers.is_empty()).then(|| self.render_rail(&t, cx));
        div()
            .id("root")
            .key_context("Root")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .relative()
            .size_full()
            .font_family(FONT)
            .text_size(px(14.))
            .text_color(t.text)
            .bg(t.base)
            .flex()
            .children(rail)
            .child(div().flex_1().min_w(px(0.)).h_full().child(screen))
            .when(!theater, |d| d.child(
                div()
                    .absolute()
                    .bottom_0()
                    .right(px(12.))
                    .text_size(px(10.))
                    .line_height(px(10.))
                    .text_color(t.text3)
                    .opacity(0.6)
                    .child(concat!("v", env!("CARGO_PKG_VERSION"))),
            ))
            .children(self.render_overlay(&t, cx))
            .when(!self.toasts.is_empty(), |d| {
                d.child(div().absolute().bottom(px(24.)).left_0().right_0().flex().flex_col().items_center().gap(px(8.)).children(
                    self.toasts.iter().map(|toast| {
                        div()
                            .px(px(16.))
                            .py(px(10.))
                            .max_w(px(520.))
                            .rounded(px(radius::CONTROL))
                            .bg(t.popover)
                            .border_1()
                            .border_color(t.stroke_strong)
                            .shadow_lg()
                            .child(body(toast.text.clone(), t.text))
                    }),
                ))
            })
    }
}

impl Root {
    /// The sign-in screen for the active server, which goes straight in when a sign-in is kept.
    pub fn show_connect(&mut self, error: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if let Screen::Server(v) = &self.screen {
            v.update(cx, |v, cx| v.shutdown(cx));
        }
        crate::elsewhere::elsewhere_mut(cx).home = None;
        self.then = None;
        // The overlay goes with the screen, the ringing dialog with it; `resume_rings` brings it
        // back once the next server is up.
        self.away_ringing = None;
        self.screen_subs.clear();
        let view = cx.new(|cx| ConnectView::new(error, window, cx));
        self.subscribe_connect(&view, window, cx);
        self.screen = Screen::Connect { view };
        self.overlay = Overlay::default();
        cx.notify();
    }

    fn subscribe_connect(&mut self, view: &Entity<ConnectView>, window: &mut Window, cx: &mut Context<Self>) {
        let sub = cx.subscribe_in(view, window, |this, _, ev: &ConnectEvent, window, cx| match ev {
            ConnectEvent::Connected(done) => this.enter(done, window, cx),
        });
        self.screen_subs.push(sub);
    }

    /// Starts the session a sign-in handed over, in place of whatever was open. A server that was
    /// kept in the background goes on with the session it had, so a call ringing there still rings.
    fn enter(&mut self, done: &Connected, window: &mut Window, cx: &mut Context<Self>) {
        if let Screen::Server(v) = &self.screen {
            v.update(cx, |v, cx| v.shutdown(cx));
        }
        let (base, nickname) = (done.api.base(), done.me.nickname.clone());
        let session = match self.take_away(&base, &nickname, cx) {
            Some(session) => {
                session.update(cx, |s, cx| {
                    s.api.set_token(&done.api.token());
                    s.promote(done.me.clone(), done.ice_servers.clone(), cx);
                });
                session
            }
            None => Session::start(
                done.api.clone(),
                done.cache.clone(),
                done.me.clone(),
                done.server_name.clone(),
                done.server_icon.clone(),
                done.ice_servers.clone(),
                cx,
            ),
        };
        self.then = match self.pending.take() {
            Some((url, user, then)) if user == nickname && crate::core::settings::same_server(&url, &base) => Some(then),
            _ => None,
        };
        self.open_server(session, window, cx);
        self.sync_elsewhere(window, cx);
        self.resume_rings(window, cx);
    }

    fn open_server(&mut self, session: Entity<Session>, window: &mut Window, cx: &mut Context<Self>) {
        self.screen_subs.clear();
        crate::elsewhere::elsewhere_mut(cx).home = Some(session.clone());
        // What `go_to` came here to do, once the socket is up.
        self.screen_subs.push(cx.observe_in(&session, window, |this, session, window, cx| {
            if session.read(cx).link != Link::Up {
                return;
            }
            // After this update: what it does may toast or open a dialog, which go through the root.
            if let (Some(then), Screen::Server(v)) = (this.then.take(), &this.screen) {
                let v = v.clone();
                window.defer(cx, move |window, cx| v.update(cx, |v, cx| v.carry_out(then, window, cx)));
            }
        }));
        let sub = cx.subscribe_in(&session, window, |this, _, ev: &SessionEvent, window, cx| match ev {
            SessionEvent::SignedOut(why) => {
                crate::prefs::set_prefs(cx, |s| s.session_token.clear());
                this.show_connect(Some(why.message().into()), window, cx);
            }
            SessionEvent::Toast(text) => this.toast(text.clone(), cx),
            _ => {}
        });
        self.screen_subs.push(sub);
        self.screen_subs.push(cx.observe(&session, |_, session, cx| rail::keep_session_news(&session, cx)));
        let root = cx.entity().downgrade();
        let view = cx.new(|cx| ServerView::new(session, root, window, cx));
        self.screen = Screen::Server(view);
        cx.notify();
    }
}
