//! A person's card, opened from their picture or their name: what everybody on the server may
//! know about them (name, username, rank, since when, whether they are around and where), their
//! picture large, and the ways to talk to them.

use super::ServerView;
use crate::core::types::*;
use crate::session::Session;
use crate::theme::{Theme, current, px, radius};
use crate::ui::overlay::{self, Dismiss, dialog_card};
use crate::widgets::*;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Pixels, Point, RenderImage, SharedString, StatefulInteractiveElement, Styled, StyledImage, Subscription, WeakEntity,
    Window, div,
};
use std::sync::Arc;

const WIDTH: f32 = 300.;
const PICTURE: f32 = 84.;
const BANNER: f32 = 64.;

impl ServerView {
    /// The card of `user` on `session`'s server, with its corner at `at`. `session` may be another
    /// server's, for someone in a private conversation from there.
    pub fn open_profile(&mut self, session: Entity<Session>, user: UserId, at: Point<Pixels>, cx: &mut Context<Self>) {
        let home = session == self.session;
        let server = cx.entity().downgrade();
        let card = cx.new(|cx| ProfileCard::new(session, server, user, home, cx));
        overlay::open_menu(card, at, cx);
    }
}

pub struct ProfileCard {
    session: Entity<Session>,
    server: WeakEntity<ServerView>,
    user: UserId,
    /// On the server this window is showing: calls are made from here. Someone from another
    /// server's private conversation can be written to, and called from over there.
    home: bool,
    _watch: Subscription,
}

impl EventEmitter<Dismiss> for ProfileCard {}

/// What the card says, read from the session in one go.
struct Facts {
    me: bool,
    name: String,
    nickname: Option<String>,
    role: Option<Role>,
    joined: Option<(String, String)>,
    online: bool,
    voice: Option<String>,
    blocked: bool,
}

impl ProfileCard {
    fn new(session: Entity<Session>, server: WeakEntity<ServerView>, user: UserId, home: bool, cx: &mut Context<Self>) -> ProfileCard {
        // A new picture, a new name, someone coming online or joining a channel: the card follows.
        let watch = cx.observe(&session, |_, _, cx| cx.notify());
        ProfileCard { session, server, user, home, _watch: watch }
    }

    fn facts(&self, cx: &App) -> Facts {
        let s = self.session.read(cx);
        let me = s.me.id == self.user;
        let user = if me { Some(&s.me) } else { s.users.get(&self.user) };
        let voice = s
            .rosters
            .iter()
            .find(|(_, roster)| roster.iter().any(|m| m.user_id == self.user))
            .and_then(|(id, _)| s.channel(*id))
            .map(|c| c.name.clone());
        Facts {
            me,
            name: s.display_name(Some(self.user), user.map(|u| u.nickname.as_str())),
            nickname: user.map(|u| u.nickname.clone()),
            role: user.map(|u| u.role),
            joined: user.filter(|u| u.created_at > 0).map(|u| crate::ui::settings::profile::joined(u.created_at)),
            online: me || s.online.contains(&self.user),
            voice,
            blocked: s.blocked.contains(&self.user),
        }
    }

    /// Runs `f` on the server view, after the card has closed.
    fn then(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut ServerView, Entity<Session>, &mut Window, &mut Context<ServerView>) + 'static,
    ) {
        cx.emit(Dismiss);
        let (server, session, handle) = (self.server.clone(), self.session.clone(), window.window_handle());
        cx.defer(move |cx| {
            let Some(server) = server.upgrade() else { return };
            let _ = handle.update(cx, |_, window, cx| server.update(cx, |v, cx| f(v, session, window, cx)));
        });
    }

    fn show_picture(&mut self, image: Arc<RenderImage>, name: String, window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(Dismiss);
        let view = cx.new(|cx| Picture { image, name: name.into(), focus: cx.focus_handle() });
        overlay::open_dialog(view, window, cx);
    }

    fn picture(&self, f: &Facts, image: Option<Arc<RenderImage>>, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let status = if f.voice.is_some() { t.accent } else { t.success };
        let mut frame = div()
            .id("profile-picture")
            .relative()
            .flex_none()
            .p(px(4.))
            .rounded_full()
            .bg(t.popover)
            .child(avatar(&f.name, image.clone(), PICTURE, None))
            .when(f.online, |d| {
                d.child(
                    div()
                        .absolute()
                        .right(px(4.))
                        .bottom(px(4.))
                        .size(px(18.))
                        .rounded_full()
                        .border_2()
                        .border_color(t.popover)
                        .bg(status),
                )
            });
        if let Some(image) = image {
            let name = f.name.clone();
            frame = frame
                .cursor_pointer()
                .tooltip(tip(tr!("See the picture", "Ver a foto"), t))
                .on_click(cx.listener(move |this, _, window, cx| this.show_picture(image.clone(), name.clone(), window, cx)));
        }
        frame.into_any_element()
    }

    fn fact(
        glyph: &'static str,
        title: impl Into<SharedString>,
        value: impl Into<SharedString>,
        note: Option<String>,
        t: &Theme,
    ) -> gpui::Div {
        div().flex().gap(px(10.)).child(div().pt(px(2.)).child(icon(glyph, 14., t.text3))).child(
            div()
                .flex()
                .flex_col()
                .min_w(px(0.))
                .gap(px(1.))
                .child(div().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(t.text3).child(title.into()))
                .child(div().text_size(px(13.)).text_color(t.text).child(value.into()))
                .when_some(note, |d, n| d.child(div().text_size(px(12.)).text_color(t.text2).child(n))),
        )
    }
}

impl gpui::Render for ProfileCard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = current();
        let f = self.facts(cx);
        let user = self.user;
        let image = self.session.update(cx, |s, cx| s.avatar(user, cx));

        let rank = match f.role {
            Some(Role::Owner) => Some(("crown", tr!("Owner of the server", "Dono do servidor"), t.caution)),
            Some(Role::Admin) => Some(("shield", tr!("Admin", "Admin"), t.accent)),
            _ => None,
        };
        let presence = match (&f.voice, f.online) {
            (Some(channel), _) => trf!("In voice · {}", "Em voz · {}", channel),
            (None, true) => tr!("Online", "Online").to_string(),
            (None, false) => tr!("Offline", "Offline").to_string(),
        };

        let names = div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .px(px(16.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(div().truncate().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).text_color(t.text).child(f.name.clone()))
                    .when_some(rank, |d, (glyph, text, color)| {
                        d.child(div().id("profile-rank").child(icon(glyph, 15., color)).tooltip(tip(text, &t)))
                    }),
            )
            .when_some(f.nickname.clone(), |d, n| d.child(div().truncate().text_size(px(13.)).text_color(t.text2).child(format!("@{n}"))));

        let facts = div()
            .mx(px(12.))
            .p(px(12.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .rounded(px(radius::CONTROL + 2.))
            .bg(t.layer)
            .child(Self::fact(if f.voice.is_some() { "headphones" } else { "radio" }, tr!("Now", "Agora"), presence, None, &t))
            .when_some(f.joined.clone(), |d, (date, ago)| {
                d.child(Self::fact("clock", tr!("Member since", "Membro desde"), date, Some(ago), &t))
            })
            .when_some(rank, |d, (glyph, text, _)| d.child(Self::fact(glyph, tr!("Role", "Cargo"), text, None, &t)))
            .when(f.blocked, |d| {
                d.child(Self::fact(
                    "ban",
                    tr!("Blocked", "Bloqueado"),
                    tr!("You blocked this person", "Você bloqueou esta pessoa"),
                    None,
                    &t,
                ))
            });

        let no_messages = matches!(self.session.read(cx).vault.state, crate::dm::KeyState::Unsupported);
        let actions = if no_messages && !f.me {
            div()
                .flex()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .py(px(4.))
                .child(icon("lock", 12., t.text3))
                .child(caption(tr!("This server has no private messages.", "Este servidor não tem mensagens privadas."), t.text3))
        } else if f.me {
            div().child(
                icon_label_button("profile-edit", "edit", tr!("Edit your profile", "Editar seu perfil"), Kind::Standard, &t)
                    .w_full()
                    .justify_center()
                    .on_click(cx.listener(|this, _, window, cx| this.then(window, cx, |v, _, window, cx| v.open_settings(window, cx)))),
            )
        } else {
            let home = self.home;
            div()
                .flex()
                .gap(px(8.))
                .child(
                    icon_label_button("profile-message", "message", tr!("Message", "Mensagem"), Kind::Primary, &t)
                        .flex_1()
                        .justify_center()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.then(window, cx, move |v, session, window, cx| {
                                if home { v.message_user(user, window, cx) } else { v.message_away(session, user, window, cx) }
                            })
                        })),
                )
                .when(home, |d| {
                    d.child(
                        icon_label_button("profile-call", "phone", tr!("Call", "Ligar"), Kind::Standard, &t)
                            .flex_1()
                            .justify_center()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.then(window, cx, move |v, _, window, cx| v.call_user(user, window, cx))
                            })),
                    )
                })
        };

        div()
            .id("profile-card")
            .w(px(WIDTH))
            .flex()
            .flex_col()
            .rounded(px(radius::PANE + 2.))
            .bg(t.popover)
            .border_1()
            .border_color(t.stroke_strong)
            .shadow_lg()
            .overflow_hidden()
            .occlude()
            // Their colour, dimmed, so the initials on it in full stand out from it. Rounded itself:
            // `overflow_hidden` clips to the square, not to the card's corners.
            .child(div().h(px(BANNER)).rounded_t(px(radius::PANE + 1.)).bg(name_color(&f.name).opacity(0.55)))
            .child(div().mt(px(-(PICTURE / 2. + 4.))).px(px(12.)).flex().child(self.picture(&f, image, &t, cx)))
            .child(div().pt(px(6.)).pb(px(12.)).child(names))
            .child(facts)
            .child(div().p(px(12.)).child(actions))
    }
}

/// A person's picture as large as it was uploaded, which is larger than anywhere else it shows.
struct Picture {
    image: Arc<RenderImage>,
    name: SharedString,
    focus: FocusHandle,
}

impl EventEmitter<Dismiss> for Picture {}

impl Focusable for Picture {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl gpui::Render for Picture {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = current();
        dialog_card(&t, 360.)
            .track_focus(&self.focus)
            .child(
                div()
                    .flex()
                    .justify_center()
                    .p(px(20.))
                    .bg(t.stage)
                    .child(gpui::img(self.image.clone()).size(px(320.)).rounded(px(radius::CARD + 2.)).object_fit(gpui::ObjectFit::Cover)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(16.))
                    .py(px(10.))
                    .child(div().flex_1().truncate().child(body(self.name.clone(), t.text2)))
                    .child(
                        button("picture-close", tr!("Close", "Fechar"), Kind::Primary, &t)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Dismiss))),
                    ),
            )
    }
}
