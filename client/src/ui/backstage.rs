//! The server of the call you are in, kept whole behind the one on screen: going to another server
//! leaves you in the voice channel or the call, as it would be if you had stayed. Its view goes on
//! looking after the call (its events, being moved, the soundpad, the end of it) with its full
//! socket open, so the server keeps your place; the server on screen shows the call in its corner,
//! and the rail marks where it is. Going back to it is instant, and the call ending lets it go to
//! the background like any other server.

use super::server::ServerView;
use super::{Root, Screen};
use crate::elsewhere::{self, Away, key};
use crate::prefs::{prefs, set_prefs};
use crate::session::SessionEvent;
use gpui::{App, Context, Entity, Global, Window};

/// The view kept behind, where the root and anything else (the tray, the hotkeys, the server on
/// screen) can find it without reaching into the root.
#[derive(Default)]
pub struct Backstage(Option<Entity<ServerView>>);

impl Global for Backstage {}

/// The server view kept behind for its call, if any.
pub fn backstage(cx: &App) -> Option<Entity<ServerView>> {
    cx.try_global::<Backstage>()?.0.clone()
}

/// The view with the call, for what runs outside the root (the tray, the hotkeys): the one on
/// screen, or else the one behind it. The root itself asks `Root::call_view`, as it cannot be
/// read while it is being updated.
pub fn call_view(cx: &App) -> Option<Entity<ServerView>> {
    let root = cx.try_global::<super::overlay::RootHandle>()?.0.upgrade()?;
    root.read(cx).call_view(cx)
}

impl Root {
    /// The view with the call: the one on screen, or else the one behind it.
    pub(super) fn call_view(&self, cx: &App) -> Option<Entity<ServerView>> {
        let front = match &self.screen {
            Screen::Server(v) => Some(v.clone()),
            Screen::Connect { .. } => None,
        };
        front.clone().filter(|v| v.read(cx).call.is_some()).or_else(|| backstage(cx)).or(front)
    }

    /// Which saved server holds the call: (address, username), as `elsewhere::key` has them.
    pub(super) fn call_server(&self, cx: &App) -> Option<(String, String)> {
        let view = self.call_view(cx)?;
        let v = view.read(cx);
        v.call.as_ref()?;
        let s = v.session.read(cx);
        Some(key(&s.api.base(), &s.me.nickname))
    }

    /// Leaving the server on screen for another: a view in a call stays behind with it, and any
    /// other goes as it always did. Not for signing out or leaving the server: the call ends then.
    pub(super) fn leave_screen(&mut self, window: &mut Window, cx: &mut Context<Root>) {
        let Screen::Server(v) = &self.screen else { return };
        let v = v.clone();
        if backstage(cx).as_ref() == Some(&v) {
            return;
        }
        let kept = {
            let view = v.read(cx);
            let s = view.session.read(cx);
            view.call.is_some() && prefs(cx).saved_server_as(&s.api.base(), &s.me.nickname).is_some()
        };
        if kept {
            self.send_backstage(v, window, cx);
        } else {
            v.update(cx, |v, cx| v.shutdown(cx));
        }
    }

    fn send_backstage(&mut self, view: Entity<ServerView>, window: &mut Window, cx: &mut Context<Root>) {
        self.release_backstage(window, cx);
        let session = view.read(cx).session.clone();
        let (url, username) = {
            let s = session.read(cx);
            (s.api.base(), s.me.nickname.clone())
        };
        view.update(cx, |v, cx| v.set_backstage(true, window, cx));
        elsewhere::elsewhere_mut(cx).away.push(Away { session: session.clone(), url, username, held: true });
        self.backstage_subs = vec![
            // The call over: back to the background like any server. Until then, the corner of
            // the server on screen follows it.
            cx.observe_in(&view, window, |root, view, window, cx| {
                if view.read(cx).call.is_none() {
                    root.release_backstage(window, cx);
                } else if let Screen::Server(front) = &root.screen {
                    front.update(cx, |_, cx| cx.notify());
                }
            }),
            cx.subscribe_in(&session, window, |root, session, ev: &SessionEvent, window, cx| match ev {
                SessionEvent::SignedOut(_) => {
                    let (url, user) = {
                        let s = session.read(cx);
                        (s.api.base(), s.me.nickname.clone())
                    };
                    set_prefs(cx, |p| p.forget_token(&url, &user));
                    root.release_backstage(window, cx);
                }
                SessionEvent::Toast(text) => root.toast(text.clone(), cx),
                _ => {}
            }),
        ];
        cx.set_global(Backstage(Some(view)));
        cx.notify();
    }

    /// Ends what was kept behind: hangs up if the call is still on, closes its server, and lets
    /// the background start it again as any other.
    pub(super) fn release_backstage(&mut self, window: &mut Window, cx: &mut Context<Root>) {
        let Some(view) = backstage(cx) else { return };
        cx.set_global(Backstage(None));
        self.backstage_subs.clear();
        let id = view.read(cx).session.entity_id();
        view.update(cx, |v, cx| v.shutdown(cx));
        elsewhere::elsewhere_mut(cx).away.retain(|a| a.session.entity_id() != id);
        if matches!(self.screen, Screen::Server(_)) {
            self.sync_elsewhere(window, cx);
        }
        self.redraw_with_away(cx);
    }

    /// Back to the server kept behind, as it was: the one on screen goes, as any other would.
    /// Says whether `url` and `username` were that server.
    pub(super) fn bring_back(&mut self, url: &str, username: &str, window: &mut Window, cx: &mut Context<Root>) -> bool {
        let Some(view) = backstage(cx) else { return false };
        let session = view.read(cx).session.clone();
        if key(&session.read(cx).api.base(), &session.read(cx).me.nickname) != key(url, username) {
            return false;
        }
        let mut found = false;
        set_prefs(cx, |p| found = p.switch_server(url, username));
        if !found {
            return false;
        }
        if let Screen::Server(front) = &self.screen
            && *front != view
        {
            front.update(cx, |v, cx| v.shutdown(cx));
        }
        cx.set_global(Backstage(None));
        self.backstage_subs.clear();
        elsewhere::elsewhere_mut(cx).away.retain(|a| a.session.entity_id() != session.entity_id());
        // As `show_connect` does with the screen it replaces.
        self.away_ringing = None;
        self.overlay = super::overlay::Overlay::default();
        view.update(cx, |v, cx| v.set_backstage(false, window, cx));
        self.then = None;
        let then = match self.pending.take() {
            Some((u, n, then)) if key(&u, &n) == key(url, username) => Some(then),
            _ => None,
        };
        self.show_server(view.clone(), session, window, cx);
        self.sync_elsewhere(window, cx);
        self.resume_rings(window, cx);
        if let Some(then) = then {
            // After this update: what it does may toast or open a dialog, which go through the root.
            window.defer(cx, move |window, cx| view.update(cx, |v, cx| v.carry_out(then, window, cx)));
        }
        true
    }
}
