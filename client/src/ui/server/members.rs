//! Everyone on the server, online first, with their role; the owner's and admins' menus for
//! promoting and removing people.

use super::ServerView;
use super::sidebar::{Menu, MenuEntry};
use crate::core::types::*;
use crate::session::Session;
use crate::theme::{Theme, current, px, radius};
use crate::ui::overlay::{self, Ask, Field};
use crate::widgets::*;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, ClickEvent, Context, Entity, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement,
    StatefulInteractiveElement, Styled, WeakEntity, Window, div, uniform_list,
};
use std::cell::RefCell;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

/// Every line of the member list is this tall, headings included: the list draws only the lines
/// in view, which takes them all the same height. A row is 36 px with 1 px under it.
const LINE: f32 = 37.;

/// What decides who is listed, in what order and where: everyone's id, names and whether they
/// are online. Summed rather than chained, as the maps iterate in no set order. Hashing every
/// name is far cheaper than what it spares, copying and sorting everybody, and the session
/// notifies for every message.
pub(super) fn people_key(s: &Session) -> u64 {
    let mut key = s.users.len() as u64;
    for u in s.users.values() {
        let mut h = DefaultHasher::new();
        (u.id, u.name(), &u.nickname, s.online.contains(&u.id)).hash(&mut h);
        key = key.wrapping_add(h.finish());
    }
    key
}

#[derive(Clone, Copy)]
enum Line {
    Heading { online: bool, count: usize },
    Member { id: UserId, online: bool },
}

/// The member list's lines, made again only when `people_key` changes.
struct Roster {
    session: WeakEntity<Session>,
    key: u64,
    lines: Rc<Vec<Line>>,
    online: usize,
    total: usize,
}

thread_local! {
    /// By session: the list is drawn for the server view, which keeps nothing of it.
    static ROSTERS: RefCell<Vec<Roster>> = const { RefCell::new(Vec::new()) };
}

/// Online first, then offline, each by name, under a heading with how many.
fn roster(session: &Entity<Session>, cx: &App) -> (Rc<Vec<Line>>, usize, usize) {
    let s = session.read(cx);
    let key = people_key(s);
    let id = session.entity_id();
    ROSTERS.with(|r| {
        let mut r = r.borrow_mut();
        if let Some(known) = r.iter().find(|k| k.session.entity_id() == id && k.key == key) {
            return (known.lines.clone(), known.online, known.total);
        }
        r.retain(|k| k.session.entity_id() != id && k.session.upgrade().is_some());
        let mut users: Vec<(String, UserId)> = s.users.values().map(|u| (u.name().to_lowercase(), u.id)).collect();
        users.sort();
        let (online, offline): (Vec<UserId>, Vec<UserId>) = users.into_iter().map(|(_, id)| id).partition(|id| s.online.contains(id));
        let mut lines = vec![Line::Heading { online: true, count: online.len() }];
        lines.extend(online.iter().map(|&id| Line::Member { id, online: true }));
        if !offline.is_empty() {
            lines.push(Line::Heading { online: false, count: offline.len() });
            lines.extend(offline.iter().map(|&id| Line::Member { id, online: false }));
        }
        let known =
            Roster { session: session.downgrade(), key, lines: Rc::new(lines), online: online.len(), total: online.len() + offline.len() };
        let out = (known.lines.clone(), known.online, known.total);
        r.push(known);
        out
    })
}

impl ServerView {
    pub fn render_members(&mut self, t: &Theme, _: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let (lines, online_count, total) = roster(&self.session, cx);
        // Only the lines in view are made, so only their pictures are asked for.
        let list = uniform_list(
            "members",
            lines.len(),
            cx.processor(move |this, range: std::ops::Range<usize>, window, cx| {
                let t = current();
                range.map(|ix| this.member_line(lines[ix], &t, window, cx)).collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .min_h(px(0.))
        .px(px(8.))
        .pb(px(10.));
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(44.))
                    .px(px(14.))
                    .child(label(tr!("Members", "Membros"), t))
                    .child(mono(format!("{online_count}/{total}"), t.text3)),
            )
            .child(list)
    }

    fn member_line(&mut self, line: Line, t: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let content = match line {
            Line::Heading { online, count } => div()
                .size_full()
                .flex()
                .items_end()
                .px(px(6.))
                .pb(px(4.))
                .child(label(
                    if online { trf!("Online · {}", "Online · {}", count) } else { trf!("Offline · {}", "Offline · {}", count) },
                    t,
                ))
                .into_any_element(),
            Line::Member { id, online } => {
                let s = self.session.read(cx);
                let in_voice = online && s.rosters.values().flatten().any(|m| m.user_id == id);
                match s.users.get(&id).cloned() {
                    Some(u) => self.member_row(&u, online, in_voice, t, window, cx),
                    None => div().into_any_element(),
                }
            }
        };
        div().w_full().h(px(LINE)).pb(px(1.)).child(content).into_any_element()
    }

    fn member_row(&mut self, u: &User, online: bool, in_voice: bool, t: &Theme, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let me = self.session.read(cx).me.clone();
        let img = self.session.update(cx, |s, cx| s.avatar(u.id, cx));
        let user = u.clone();
        let hover = t.layer_hover;
        div()
            .id(("member", u.id as u64))
            .flex()
            .items_center()
            .gap(px(10.))
            .h(px(36.))
            .px(px(6.))
            .rounded(px(radius::INNER + 1.))
            .hover(move |s| s.bg(hover))
            .when(!online, |d| d.opacity(0.45))
            .child(div().relative().child(avatar(u.name(), img, 26., None)).when(online, |d| {
                d.child(
                    div()
                        .absolute()
                        .right(px(-2.))
                        .bottom(px(-2.))
                        .size(px(10.))
                        .rounded_full()
                        .border_2()
                        .border_color(t.pane)
                        .bg(if in_voice { t.accent } else { t.success }),
                )
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .child(div().truncate().text_size(px(13.5)).text_color(t.text).child(u.name().to_string()))
                    .when(u.custom_name, |d| {
                        d.child(div().truncate().text_size(px(11.)).text_color(t.text3).child(format!("@{}", u.nickname)))
                    }),
            )
            .when(u.role == Role::Owner, |d| d.child(icon("crown", 13., t.caution)))
            .when(u.role == Role::Admin, |d| d.child(icon("shield", 13., t.accent)))
            // A click shows their card, with a message and a call a click away; for anybody but
            // you the menu has the rest.
            .cursor_pointer()
            .on_click({
                let id = u.id;
                cx.listener(move |this, e: &ClickEvent, _, cx| this.open_profile(this.session.clone(), id, e.position(), cx))
            })
            .when(u.id != me.id, |d| {
                d.on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, e: &MouseDownEvent, window, cx| this.member_menu(&user, e.position, window, cx)),
                )
            })
            .into_any_element()
    }

    fn member_menu(&mut self, u: &User, at: gpui::Point<gpui::Pixels>, _: &mut Window, cx: &mut Context<Self>) {
        let me = self.session.read(cx).me.clone();
        let session = self.session.clone();
        let (id, nick) = (u.id, u.nickname.clone());
        let view = cx.entity().downgrade();
        let (v1, v2) = (view.clone(), view);
        // A server older than private messages has no way to write to anyone.
        let mut items = if self.no_messages(cx) {
            Vec::new()
        } else {
            vec![
                MenuEntry::item("message", tr!("Send a private message", "Mandar mensagem privada"), false, move |window, cx| {
                    if let Some(v) = v1.upgrade() {
                        v.update(cx, |v, cx| v.message_user(id, window, cx));
                    }
                }),
                MenuEntry::item("phone", tr!("Call", "Ligar"), false, move |window, cx| {
                    if let Some(v) = v2.upgrade() {
                        v.update(cx, |v, cx| v.call_user(id, window, cx));
                    }
                }),
            ]
        };
        if !me.role.is_admin() || u.role == Role::Owner {
            if items.is_empty() {
                return;
            }
            let menu = cx.new(|_| Menu { items });
            overlay::open_menu(menu, at, cx);
            return;
        }
        if !items.is_empty() && (u.role == Role::Member || me.role == Role::Owner) {
            items.push(MenuEntry::rule());
        }
        if u.role == Role::Member {
            let session = session.clone();
            items.push(MenuEntry::item("shield", tr!("Make admin", "Tornar admin"), false, move |_, cx| {
                session
                    .update(cx, |s, cx| s.call(cx, move |api| Box::pin(async move { api.set_role(id, Role::Admin).await }), |_, _, _| {}))
            }));
        } else if me.role == Role::Owner {
            let session = session.clone();
            items.push(MenuEntry::item("shield-off", tr!("Remove admin", "Remover admin"), false, move |_, cx| {
                session
                    .update(cx, |s, cx| s.call(cx, move |api| Box::pin(async move { api.set_role(id, Role::Member).await }), |_, _, _| {}))
            }));
        }
        if me.role == Role::Owner {
            items.push(MenuEntry::rule());
            items.push(MenuEntry::item("delete", tr!("Delete account", "Apagar conta"), true, move |window, cx| {
                let session = session.clone();
                let nick = nick.clone();
                Ask::open(
                    trf!("Delete {}'s account?", "Apagar a conta de {}?", nick),
                    Some(
                        tr!(
                            "Their messages go too. They can register again unless the server has a password. Type their name to confirm.",
                            "As mensagens vão junto. A pessoa pode se cadastrar de novo, a menos que o servidor tenha senha. Digite o nome dela para confirmar."
                        )
                        .into(),
                    ),
                    tr!("Delete account", "Apagar conta"),
                    true,
                    vec![Field::Text { label: tr!("Name", "Nome"), value: String::new(), placeholder: "", secret: false, multiline: false, max: 32 }],
                    window,
                    cx,
                    move |v, _, cx| {
                        if v[0].trim().to_lowercase() != nick {
                            return Some(trf!("Type {} to confirm.", "Digite {} para confirmar.", nick));
                        }
                        session
                            .update(cx, |s, cx| s.call(cx, move |api| Box::pin(async move { api.delete_account(id).await }), |_, _, _| {}));
                        None
                    },
                );
            }));
        }
        if items.is_empty() {
            return;
        }
        let menu = cx.new(|_| Menu { items });
        overlay::open_menu(menu, at, cx);
    }
}
