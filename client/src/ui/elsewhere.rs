//! The root's side of the servers kept in the background: starting one for each server of the
//! account in use that can be kept (any but the one on screen), dropping those that leave the rail,
//! and what they say: a private message rings and shows in the taskbar, a call opens the "is
//! calling you" dialog, and answering it goes to that server first.

use super::Root;
use super::server::Then;
use super::server::private::Incoming;
use crate::core::api::Api;
use crate::core::cache::Cache;
use crate::core::settings::{SavedServer, same_server};
use crate::core::types::*;
use crate::core::{self};
use crate::elsewhere::{self, Away, key};
use crate::media::audio::Cue;
use crate::media::voice::cue_volume;
use crate::prefs::{prefs, set_prefs};
use crate::session::{Session, SessionEvent};
use gpui::{AppContext, Context, Entity, Window};

/// A burst of messages from the background makes one sound.
const CUE_BURST: std::time::Duration = std::time::Duration::from_secs(2);

impl Root {
    /// Keeps a background session for every server of the account in use with a kept sign-in, but
    /// the one on screen; one that left the rail, or belongs to another account, is let go.
    pub(super) fn sync_elsewhere(&mut self, window: &mut Window, cx: &mut Context<Root>) {
        let p = prefs(cx);
        let home = elsewhere::elsewhere(cx).and_then(|e| e.home.clone()).map(|s| {
            let s = s.read(cx);
            key(&s.api.base(), &s.me.nickname)
        });
        let wanted: Vec<SavedServer> = p
            .account_servers(&p.active_account)
            .filter(|(_, s)| !s.session_token.is_empty() && Some(key(&s.url, &s.username)) != home)
            .map(|(_, s)| s.clone())
            .collect();
        let gone: Vec<Entity<Session>> = elsewhere::elsewhere(cx)
            .map(|e| {
                e.away
                    .iter()
                    .filter(|a| !wanted.iter().any(|s| key(&s.url, &s.username) == key(&a.url, &a.username)))
                    .map(|a| a.session.clone())
                    .collect()
            })
            .unwrap_or_default();
        for session in gone {
            self.drop_away(&session, cx);
        }
        for s in wanted {
            let k = key(&s.url, &s.username);
            let e = elsewhere::elsewhere_mut(cx);
            if e.unsupported.contains(&k) || e.starting.contains(&k) || e.away.iter().any(|a| key(&a.url, &a.username) == k) {
                continue;
            }
            e.starting.insert(k.clone());
            self.start_away(s, k, window, cx);
        }
    }

    /// Asks a server whether it can be kept in the background and who you are there, then keeps it.
    fn start_away(&mut self, s: SavedServer, k: (String, String), window: &mut Window, cx: &mut Context<Root>) {
        let api = Api::new();
        api.set_server(&s.url, &s.password);
        api.set_token(&s.session_token);
        let cache = Cache::shared(prefs(cx).media_cache_mb);
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let probe = api.clone();
            let got = core::run(async move {
                let health = probe.health().await?;
                if !health.has("inbox") {
                    return Ok(None);
                }
                probe.me().await.map(Some)
            })
            .await;
            let _ = handle.update(cx, |_, window, cx| {
                elsewhere::elsewhere_mut(cx).starting.remove(&k);
                // Gone from the rail, or on screen now, while it answered.
                let p = prefs(cx);
                let still = p.saved_server_as(&s.url, &s.username).is_some_and(|x| !x.session_token.is_empty())
                    && p.active_account == s.username
                    && !(same_server(&p.server_url, &s.url) && p.username == s.username);
                match got {
                    Ok(Some(me)) if still => {
                        let icon = (!s.icon.is_empty()).then(|| s.icon.clone());
                        let session = Session::start_background(api, cache, me, s.name.clone(), icon, cx);
                        let _ = this.update(cx, |root, cx| root.adopt_away(session, s.url.clone(), s.username.clone(), window, cx));
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        elsewhere::elsewhere_mut(cx).unsupported.insert(k);
                    }
                    Err(e) if e.status == 401 => set_prefs(cx, |p| p.forget_token(&s.url, &s.username)),
                    // Unreachable for now: asked again on the next round.
                    Err(e) => log::info!("{} is not kept in the background for now: {e}", s.url),
                }
            });
        })
        .detach();
    }

    fn adopt_away(&mut self, session: Entity<Session>, url: String, username: String, window: &mut Window, cx: &mut Context<Root>) {
        let id = session.entity_id();
        let subs = vec![
            cx.observe(&session, |this, _, cx| this.redraw_with_away(cx)),
            cx.subscribe_in(&session, window, |this, session, ev: &SessionEvent, window, cx| this.on_away_event(session, ev, window, cx)),
        ];
        self.away_subs.insert(id, subs);
        elsewhere::elsewhere_mut(cx).away.push(Away { session, url, username });
        self.redraw_with_away(cx);
    }

    /// Lets a background session go: closes what it has open and stops listening to it.
    pub(super) fn drop_away(&mut self, session: &Entity<Session>, cx: &mut Context<Root>) {
        let id = session.entity_id();
        self.away_subs.remove(&id);
        elsewhere::elsewhere_mut(cx).away.retain(|a| a.session.entity_id() != id);
        session.update(cx, |s, _| s.stop());
        if let Some((ringing, view)) = self.away_ringing.take() {
            if ringing == id {
                view.update(cx, |v, cx| v.close(cx));
            } else {
                self.away_ringing = Some((ringing, view));
            }
        }
        self.redraw_with_away(cx);
    }

    /// The rail's badges and the conversation list draw from the background sessions too.
    fn redraw_with_away(&mut self, cx: &mut Context<Root>) {
        if let super::Screen::Server(v) = &self.screen {
            v.update(cx, |_, cx| cx.notify());
        }
        cx.notify();
    }

    /// The background session of a server, made the one on screen: it keeps its socket until the
    /// full one is up, so a call ringing there goes on ringing.
    pub(super) fn take_away(&mut self, url: &str, username: &str, cx: &mut Context<Root>) -> Option<Entity<Session>> {
        let session = elsewhere::away_for(url, username, cx)?;
        let id = session.entity_id();
        self.away_subs.remove(&id);
        elsewhere::elsewhere_mut(cx).away.retain(|a| a.session.entity_id() != id);
        // Its ring is the server view's now (see `resume_rings`).
        if self.away_ringing.as_ref().is_some_and(|(ringing, _)| *ringing == id)
            && let Some((_, view)) = self.away_ringing.take()
        {
            view.update(cx, |v, cx| v.close(cx));
        }
        Some(session)
    }

    /// After going to a server: a call still ringing anywhere rings again, on the server now on
    /// screen or on one in the background. The dialog that rang went with the screen before.
    pub(super) fn resume_rings(&mut self, window: &mut Window, cx: &mut Context<Root>) {
        let me = |s: &Session| {
            s.calls
                .values()
                .filter(|c| c.state == CallPhase::Ringing && c.callee_id == s.me.id)
                .map(|c| c.conversation_id)
                .collect::<Vec<_>>()
        };
        if let super::Screen::Server(v) = &self.screen {
            let answering = match self.then {
                Some(Then::Answer(id)) => Some(id),
                _ => None,
            };
            let ringing: Vec<ConversationId> = me(v.read(cx).session.read(cx)).into_iter().filter(|id| Some(*id) != answering).collect();
            if let Some(id) = ringing.first().copied() {
                let v = v.clone();
                // Dialogs open through the root, which is busy running this.
                window.defer(cx, move |window, cx| v.update(cx, |v, cx| v.show_ringing(id, window, cx)));
            }
        }
        if self.away_ringing.is_none() {
            let away: Vec<Entity<Session>> =
                elsewhere::elsewhere(cx).map(|e| e.away.iter().map(|a| a.session.clone()).collect()).unwrap_or_default();
            if let Some((session, id)) = away.into_iter().find_map(|s| me(s.read(cx)).first().copied().map(|id| (s, id))) {
                self.away_ringing(&session, id, window, cx);
            }
        }
    }

    fn on_away_event(&mut self, session: &Entity<Session>, ev: &SessionEvent, window: &mut Window, cx: &mut Context<Root>) {
        match ev {
            SessionEvent::Message(Room::Dm(id), m) => {
                let s = session.read(cx);
                if m.user_id == Some(s.me.id) {
                    return;
                }
                let looking = s.viewing == Some(Room::Dm(*id)) && window.is_window_active();
                if looking {
                    return;
                }
                let name = s.display_name(m.user_id, None);
                let text = trf!("sent you a private message on {}", "te mandou uma mensagem privada em {}", s.server_name);
                crate::ui::tray::attention(window, &name, &text, cx);
                if prefs(cx).mention_sound && self.away_cue_at.is_none_or(|at| at.elapsed() >= CUE_BURST) {
                    self.away_cue_at = Some(std::time::Instant::now());
                    crate::media::audio::cue(Cue::Mention, cue_volume(cx));
                }
            }
            SessionEvent::Ringing(conversation) => self.away_ringing(session, *conversation, window, cx),
            SessionEvent::CallChanged(conversation) => {
                let still = session.read(cx).calls.get(conversation).is_some_and(|c| c.state == CallPhase::Ringing);
                if !still && let Some((id, view)) = self.away_ringing.take() {
                    if id == session.entity_id() && view.read(cx).conversation == *conversation {
                        view.update(cx, |v, cx| v.close(cx));
                    } else {
                        self.away_ringing = Some((id, view));
                    }
                }
            }
            SessionEvent::SignedOut(_) => {
                let (url, user) = {
                    let s = session.read(cx);
                    (s.api.base(), s.me.nickname.clone())
                };
                set_prefs(cx, |p| p.forget_token(&url, &user));
                self.drop_away(session, cx);
            }
            SessionEvent::Toast(text) => self.toast(text.clone(), cx),
            _ => {}
        }
    }

    /// Someone on a server in the background is calling: the same dialog as on the server on
    /// screen. Answering goes to that server and picks up there.
    fn away_ringing(&mut self, session: &Entity<Session>, conversation: ConversationId, window: &mut Window, cx: &mut Context<Root>) {
        let Some(caller) = session.read(cx).calls.get(&conversation).map(|c| c.caller_id) else { return };
        if let Some((_, old)) = self.away_ringing.take() {
            old.update(cx, |r, cx| r.close(cx));
        }
        let root = cx.entity().downgrade();
        let (url, user) = {
            let s = session.read(cx);
            (s.api.base(), s.me.nickname.clone())
        };
        let declining = session.clone();
        let view = cx.new(|cx| Incoming {
            session: session.clone(),
            conversation,
            caller,
            on_answer: std::rc::Rc::new(move |id, window, cx| {
                if let Some(root) = root.upgrade() {
                    let (url, user) = (url.clone(), user.clone());
                    root.update(cx, |r, cx| {
                        r.away_ringing = None;
                        r.go_to(&url, &user, Then::Answer(id), window, cx);
                    });
                }
            }),
            on_decline: std::rc::Rc::new(move |id, cx| {
                crate::ui::server::request(&declining, "call:hang-up", serde_json::json!({ "conversationId": id }), cx);
            }),
            focus: cx.focus_handle(),
        });
        let id = session.entity_id();
        cx.subscribe(&view, |this, v, _: &super::overlay::Dismiss, _| {
            if this.away_ringing.as_ref().is_some_and(|(_, r)| *r == v) {
                this.away_ringing = None;
            }
        })
        .detach();
        self.away_ringing = Some((id, view.clone()));
        crate::media::audio::cue(Cue::Ring, cue_volume(cx).max(0.6));
        let s = session.read(cx);
        let name = s.display_name(Some(caller), None);
        let text = trf!("is calling you on {}", "está te ligando em {}", s.server_name);
        crate::ui::tray::attention(window, &name, &text, cx);
        // Dialogs open through the root, which is busy running this.
        window.defer(cx, move |window, cx| super::overlay::open_dialog(view, window, cx));
    }

    /// Goes to another server (and does `then` there): it ends the voice channel or call you are
    /// in, so it asks first, unless that was turned off.
    pub fn ask_go_to(&mut self, url: &str, username: &str, then: Option<Then>, window: &mut Window, cx: &mut Context<Root>) {
        let p = prefs(cx);
        let here = p.username == username && same_server(&p.server_url, url);
        let place = match &self.screen {
            super::Screen::Server(v) => v.read(cx).call.as_ref().map(|c| {
                let place = c.read(cx).place;
                v.read(cx).session.read(cx).place_name(place)
            }),
            super::Screen::Connect { .. } => None,
        };
        let go = move |root: &mut Root, url: &str, username: &str, window: &mut Window, cx: &mut Context<Root>| match then {
            Some(then) => root.go_to(url, username, then, window, cx),
            None => root.switch_server(url, username, window, cx),
        };
        let Some(place) = place.filter(|_| !here && p.confirm_server_switch) else {
            return go(self, url, username, window, cx);
        };
        let name = p.saved_server_as(url, username).map(|s| if s.name.is_empty() { s.host() } else { s.name.clone() }).unwrap_or_default();
        let (root, url, username) = (cx.entity().downgrade(), url.to_string(), username.to_string());
        // Dialogs open through the root, which is busy running this.
        window.defer(cx, move |window, cx| {
            super::overlay::Ask::open(
                trf!("Go to {}?", "Ir para {}?", name),
                Some(trf!("You will leave {}.", "Você vai sair de {}.", place)),
                tr!("Go", "Ir"),
                false,
                vec![super::overlay::Field::Check { label: tr!("Don't ask again", "Não perguntar de novo"), on: false }],
                window,
                cx,
                move |v, window, cx| {
                    if !v[0].is_empty() {
                        set_prefs(cx, |p| p.confirm_server_switch = false);
                    }
                    if let Some(root) = root.upgrade() {
                        root.update(cx, |r, cx| go(r, &url, &username, window, cx));
                    }
                    None
                },
            );
        });
    }

    /// Goes to a saved server and, once it is up, does `then` there: answer a call, ring someone,
    /// or open a conversation.
    pub fn go_to(&mut self, url: &str, username: &str, then: Then, window: &mut Window, cx: &mut Context<Root>) {
        let p = prefs(cx);
        if p.saved_server_as(url, username).is_none() {
            return;
        }
        if p.username == username && same_server(&p.server_url, url) {
            // After this update: what it does may toast or open a dialog, which go through the root.
            if let super::Screen::Server(v) = &self.screen {
                let v = v.clone();
                window.defer(cx, move |window, cx| v.update(cx, |v, cx| v.carry_out(then, window, cx)));
            }
            return;
        }
        self.pending = Some((url.to_string(), username.to_string(), then));
        self.switch_server(url, username, window, cx);
    }
}
