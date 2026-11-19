//! The year in review ("Retrospectiva"): what a server's people did together in a year, told as
//! a few slides, one thing each (the messages, the hours in voice, the games, the songs, and then
//! yours), with numbers that count up and bars that grow as each one comes in.
//!
//! The server works it all out (`server/src/retro.js`); this only asks, in this computer's time
//! zone, and tells. It is offered in December and January from the top bar, and opens by itself
//! once per server the first time it is offered.

use crate::core::types::UserId;
use crate::session::Session;
use crate::theme::{FONT, current, px, radius};
use crate::ui::overlay::Dismiss;
use crate::widgets::*;
use gpui::prelude::FluentBuilder;
use gpui::{
    Animation, AnimationExt, AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, Hsla,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window, div,
    img, linear_color_stop, linear_gradient,
};
use serde::Deserialize;
use std::time::Duration;

const W: f32 = 640.;
const H: f32 = 480.;

// ------------------------------------------------------------------------------------- what comes

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
struct Year {
    year: i32,
    since: Since,
    server: Ours,
    me: Mine,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct Since {
    voice: Option<i64>,
    games: Option<i64>,
    music: Option<i64>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
struct Ours {
    messages: u64,
    writers: u64,
    top_chatters: Vec<Count>,
    busiest_channel: Option<Channel>,
    busiest_day: Option<Day>,
    by_hour: Vec<u64>,
    night_owl: Option<Count>,
    top_reactions: Vec<Emoji>,
    reactions: u64,
    voice_ms: i64,
    top_talkers: Vec<Time>,
    top_pair: Option<Pair>,
    longest_hangout: Option<Hangout>,
    games: Vec<Game>,
    top_gamer: Option<Time>,
    songs_played: u64,
    top_songs: Vec<Song>,
    top_dj: Option<Dj>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
struct Mine {
    messages: u64,
    rank: Option<u64>,
    top_channel: Option<Channel>,
    first_message: Option<First>,
    night_messages: u64,
    top_reaction: Option<Emoji>,
    voice_ms: i64,
    top_game: Option<Game>,
    partner: Option<Time>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
struct Count {
    user_id: UserId,
    n: u64,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
struct Time {
    user_id: UserId,
    ms: i64,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct Channel {
    name: String,
    n: u64,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct Day {
    date: String,
    n: u64,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct Emoji {
    emoji: String,
    n: u64,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct Pair {
    a: UserId,
    b: UserId,
    ms: i64,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
struct Hangout {
    started_at: i64,
    ms: i64,
    people: Vec<UserId>,
    name: Option<String>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct Game {
    name: String,
    ms: i64,
    players: u64,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct Song {
    title: String,
    artist: String,
    n: u64,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct Dj {
    nickname: String,
    n: u64,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct First {
    at: i64,
    body: String,
}

// ------------------------------------------------------------------------------------- when

/// The year to look back on now: this one in December, the last one in January, none otherwise.
/// `CIRANDA_RETRO=1` offers this year's at any time, to see it before December.
pub fn season() -> Option<i32> {
    use chrono::Datelike;
    let today = chrono::Local::now().date_naive();
    match today.month() {
        12 => Some(today.year()),
        1 => Some(today.year() - 1),
        _ if std::env::var_os("CIRANDA_RETRO").is_some() => Some(today.year()),
        _ => None,
    }
}

/// This computer's clock, in minutes ahead of UTC.
fn tz_minutes() -> i32 {
    chrono::Local::now().offset().local_minus_utc() / 60
}

// ------------------------------------------------------------------------------------- the view

enum Load {
    Waiting,
    Failed(String),
    Ready(Box<Year>),
}

/// One slide, from what there is to say: a slide with nothing in it is left out.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Slide {
    Hello,
    Messages,
    When,
    NightOwl,
    Reactions,
    Voice,
    Games,
    Music,
    You,
    Bye,
}

pub struct Retro {
    session: Entity<Session>,
    year: i32,
    load: Load,
    at: usize,
    focus: FocusHandle,
}

impl EventEmitter<Dismiss> for Retro {}

impl Focusable for Retro {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Retro {
    pub fn open(session: Entity<Session>, year: i32, window: &mut Window, cx: &mut App) {
        let view = cx.new(|cx: &mut Context<Retro>| {
            let api = session.read(cx).api.clone();
            cx.spawn(async move |this, cx| {
                let got = crate::core::run(async move { api.retro(year, tz_minutes()).await }).await;
                let _ = this.update(cx, |r: &mut Retro, cx| {
                    r.load = match got.map(serde_json::from_value::<Year>) {
                        Ok(Ok(y)) => Load::Ready(Box::new(y)),
                        Ok(Err(e)) => Load::Failed(e.to_string()),
                        Err(e) => Load::Failed(e.message),
                    };
                    cx.notify();
                });
            })
            .detach();
            Retro { session, year, load: Load::Waiting, at: 0, focus: cx.focus_handle() }
        });
        crate::ui::overlay::open_dialog(view, window, cx);
    }

    fn slides(y: &Year) -> Vec<Slide> {
        let s = &y.server;
        let mut out = vec![Slide::Hello];
        if s.messages > 0 {
            out.push(Slide::Messages);
            out.push(Slide::When);
        }
        if s.night_owl.is_some() {
            out.push(Slide::NightOwl);
        }
        if !s.top_reactions.is_empty() {
            out.push(Slide::Reactions);
        }
        if s.voice_ms > 0 {
            out.push(Slide::Voice);
        }
        if !s.games.is_empty() {
            out.push(Slide::Games);
        }
        if s.songs_played > 0 {
            out.push(Slide::Music);
        }
        out.push(Slide::You);
        out.push(Slide::Bye);
        out
    }

    fn step(&mut self, by: isize, cx: &mut Context<Self>) {
        let Load::Ready(y) = &self.load else { return };
        let last = Self::slides(y).len() - 1;
        self.at = (self.at as isize + by).clamp(0, last as isize) as usize;
        cx.notify();
    }

    fn on_key(&mut self, e: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        match e.keystroke.key.as_str() {
            "right" | "space" | "enter" => self.step(1, cx),
            "left" | "backspace" => self.step(-1, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    // --------------------------------------------------------------------------------- pieces

    fn person(&mut self, id: UserId, size: f32, cx: &mut Context<Self>) -> (String, gpui::Div) {
        let name = self.session.read(cx).display_name(Some(id), None);
        let image = self.session.update(cx, |s, cx| s.avatar(id, cx));
        let face = avatar(&name, image, size, None);
        (name, face)
    }

    fn emoji(&mut self, e: &str, size: f32, cx: &mut Context<Self>) -> AnyElement {
        if let Some(name) = e.strip_prefix(':').and_then(|e| e.strip_suffix(':')) {
            let custom = self.session.read(cx).emoji(name).cloned();
            if let Some(crate::session::Picture::Ready(image)) = custom.map(|c| self.session.update(cx, |s, cx| s.picture(&c.hash, cx))) {
                return img(image).size(px(size)).into_any_element();
            }
        }
        div().text_size(px(size * 0.9)).line_height(px(size)).child(e.to_string()).into_any_element()
    }

    fn slide(&mut self, which: Slide, y: &Year, cx: &mut Context<Self>) -> AnyElement {
        let white = gpui::white();
        let soft = white.opacity(0.72);
        let s = y.server.clone();
        let since = |at: Option<i64>| at.filter(|&a| a > year_start(y.year)).map(day_of_ms);
        match which {
            Slide::Hello => {
                let server = self.session.read(cx).server_name.clone();
                column()
                    .items_center()
                    .justify_center()
                    .gap(px(14.))
                    .child(crate::ui::brand_mark(64., &current()))
                    .child(kicker(tr!("Year in review", "Retrospectiva"), soft))
                    .child(big_year(y.year, 72.))
                    .child(div().text_size(px(20.)).font_weight(FontWeight::SEMIBOLD).text_color(white).child(server))
                    .child(line(tr!("What you all did together this year.", "O que vocês fizeram juntos este ano."), soft))
                    .child(div().pt(px(18.)).child(line(tr!("→ to go on", "→ para seguir"), white.opacity(0.5))))
                    .into_any_element()
            }
            Slide::Messages => {
                let mut podium = div().flex().items_end().justify_center().gap(px(22.)).pt(px(10.));
                for (i, c) in s.top_chatters.iter().take(3).enumerate() {
                    let (name, face) = self.person(c.user_id, [64., 52., 44.][i], cx);
                    podium = podium.child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(6.))
                            .child(face)
                            .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(white).child(name))
                            .child(line(trf!("{} messages", "{} mensagens", thousands(c.n)), soft))
                            .with_animation(
                                ("podium", i),
                                Animation::new(Duration::from_millis(500 + 140 * i as u64)).with_easing(gpui::ease_out_quint()),
                                |d, p| d.opacity(p).relative().top(px(16. * (1. - p))),
                            ),
                    );
                }
                column()
                    .items_center()
                    .justify_center()
                    .gap(px(8.))
                    .child(kicker(tr!("The chat", "O chat"), soft))
                    .child(big_number(s.messages as i64, "", 76.))
                    .child(line(trf!("messages, from {} people", "mensagens, de {} pessoas", s.writers), soft))
                    .child(podium)
                    .into_any_element()
            }
            Slide::When => {
                let most = s.by_hour.iter().copied().max().unwrap_or(1).max(1) as f32;
                let peak = s.by_hour.iter().enumerate().max_by_key(|(_, n)| **n).map(|(h, _)| h).unwrap_or(0);
                let bars = div().flex().items_end().gap(px(4.)).h(px(120.)).children(s.by_hour.iter().enumerate().map(|(h, n)| {
                    let frac = *n as f32 / most;
                    let color = if h == peak { gpui::hsla(0.13, 0.95, 0.62, 1.) } else { white.opacity(0.55) };
                    div().w(px(16.)).flex().flex_col().justify_end().h_full().child(
                        div().w_full().rounded_t(px(4.)).bg(color).with_animation(
                            ("hour", h),
                            Animation::new(Duration::from_millis(700 + 18 * h as u64)).with_easing(gpui::ease_out_quint()),
                            move |d, p| d.h(px((120. * frac * p).max(2.))),
                        ),
                    )
                }));
                column()
                    .items_center()
                    .justify_center()
                    .gap(px(14.))
                    .child(kicker(tr!("When and where", "Quando e onde"), soft))
                    .when_some(s.busiest_channel.clone(), |d, c| {
                        d.child(headline(trf!("#{} was the place", "#{} foi o lugar", c.name), white))
                            .child(line(trf!("{} messages there", "{} mensagens lá", thousands(c.n)), soft))
                    })
                    .child(bars)
                    .child(line(trf!("The busiest hour: {}h", "O horário mais movimentado: {}h", peak), soft))
                    .when_some(s.busiest_day.clone(), |d, day| {
                        d.child(line(
                            trf!(
                                "The busiest day: {}, {} messages",
                                "O dia mais agitado: {}, com {} mensagens",
                                long_date(&day.date),
                                thousands(day.n)
                            ),
                            soft,
                        ))
                    })
                    .into_any_element()
            }
            Slide::NightOwl => {
                let owl = s.night_owl.clone().unwrap_or_default();
                let (name, face) = self.person(owl.user_id, 96., cx);
                column()
                    .items_center()
                    .justify_center()
                    .gap(px(12.))
                    .child(kicker(tr!("The night owl", "O corujão"), soft))
                    .child(div().text_size(px(48.)).child("🦉"))
                    .child(face)
                    .child(headline(name, white))
                    .child(line(
                        trf!("{} messages between midnight and 5 a.m.", "{} mensagens entre meia-noite e 5 da manhã", thousands(owl.n)),
                        soft,
                    ))
                    .into_any_element()
            }
            Slide::Reactions => {
                let mut row = div().flex().items_end().justify_center().gap(px(26.)).pt(px(12.));
                for (i, e) in s.top_reactions.iter().enumerate() {
                    let size = [72., 56., 48., 42., 38.][i.min(4)];
                    let face = self.emoji(&e.emoji, size, cx);
                    row = row.child(
                        div().flex().flex_col().items_center().gap(px(6.)).child(face).child(line(thousands(e.n), soft)).with_animation(
                            ("reaction", i),
                            Animation::new(Duration::from_millis(420 + 120 * i as u64)).with_easing(gpui::ease_out_quint()),
                            |d, p| d.opacity(p).relative().top(px(20. * (1. - p))),
                        ),
                    );
                }
                column()
                    .items_center()
                    .justify_center()
                    .gap(px(10.))
                    .child(kicker(tr!("Reactions", "Reações"), soft))
                    .child(big_number(s.reactions as i64, "", 64.))
                    .child(line(tr!("reactions, and the favourites were", "reações, e as favoritas foram"), soft))
                    .child(row)
                    .into_any_element()
            }
            Slide::Voice => {
                let hours = s.voice_ms / 3_600_000;
                let pair = s.top_pair.clone().map(|p| {
                    let (a, fa) = self.person(p.a, 44., cx);
                    let (b, fb) = self.person(p.b, 44., cx);
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .px(px(16.))
                        .py(px(10.))
                        .rounded(px(radius::CARD))
                        .bg(white.opacity(0.08))
                        .child(div().flex().child(fa).child(div().ml(px(-10.)).child(fb)))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(div().text_size(px(12.)).text_color(soft).child(tr!("The duo of the year", "A dupla do ano")))
                                .child(
                                    div()
                                        .text_size(px(15.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(white)
                                        .child(trf!("{} & {}", "{} & {}", a, b)),
                                )
                                .child(line(trf!("{} together in voice", "{} juntos na voz", duration(p.ms)), soft)),
                        )
                });
                let hangout = s.longest_hangout.clone().map(|h| {
                    let mut faces = div().flex();
                    for (i, id) in h.people.iter().take(6).enumerate() {
                        let (_, f) = self.person(*id, 26., cx);
                        faces = faces.child(div().when(i > 0, |d| d.ml(px(-7.))).child(f));
                    }
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .px(px(16.))
                        .py(px(10.))
                        .rounded(px(radius::CARD))
                        .bg(white.opacity(0.08))
                        .child(faces)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(div().text_size(px(12.)).text_color(soft).child(tr!("The longest hangout", "A maior resenha")))
                                .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(white).child(duration(h.ms)))
                                .child(line(
                                    match h.name {
                                        Some(n) => trf!("{} · {}", "{} · {}", day_of_ms(h.started_at), n),
                                        None => day_of_ms(h.started_at),
                                    },
                                    soft,
                                )),
                        )
                });
                column()
                    .items_center()
                    .justify_center()
                    .gap(px(12.))
                    .child(kicker(tr!("In voice", "Na voz"), soft))
                    .child(big_number(hours.max(if s.voice_ms > 0 { 1 } else { 0 }), tr!(" h", " h"), 76.))
                    .child(line(tr!("talking, playing and hanging out", "de conversa, jogatina e resenha"), soft))
                    .children(pair)
                    .children(hangout)
                    .when_some(since(y.since.voice), |d, day| d.child(fine(trf!("Counted since {}", "Contando desde {}", day), white)))
                    .into_any_element()
            }
            Slide::Games => {
                let most = s.games.first().map(|g| g.ms).unwrap_or(1).max(1) as f32;
                let mut list = div().flex().flex_col().gap(px(10.)).w(px(440.));
                for (i, g) in s.games.iter().enumerate() {
                    let frac = g.ms as f32 / most;
                    list = list.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .child(
                                        div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(white).child(g.name.clone()),
                                    )
                                    .child(line(trf!("{} · {} playing", "{} · {} jogando", duration(g.ms), g.players), soft)),
                            )
                            .child(div().h(px(8.)).rounded_full().bg(white.opacity(0.1)).child(
                                div().h_full().rounded_full().bg(game_colour(i)).with_animation(
                                    ("game", i),
                                    Animation::new(Duration::from_millis(700 + 120 * i as u64)).with_easing(gpui::ease_out_quint()),
                                    move |d, p| d.w(px(440. * frac * p)),
                                ),
                            )),
                    );
                }
                let gamer = s.top_gamer.clone().map(|g| {
                    let (name, _) = self.person(g.user_id, 20., cx);
                    line(trf!("Who played the most: {}, {}", "Quem mais jogou: {}, {}", name, duration(g.ms)), soft)
                });
                column()
                    .items_center()
                    .justify_center()
                    .gap(px(14.))
                    .child(kicker(tr!("The games", "Os jogos"), soft))
                    .child(headline(s.games.first().map(|g| trf!("{} was the game", "{} foi o jogo", g.name)).unwrap_or_default(), white))
                    .child(list)
                    .children(gamer)
                    .when_some(since(y.since.games), |d, day| d.child(fine(trf!("Counted since {}", "Contando desde {}", day), white)))
                    .into_any_element()
            }
            Slide::Music => {
                let mut list = div().flex().flex_col().gap(px(8.)).w(px(440.));
                for (i, song) in s.top_songs.iter().take(4).enumerate() {
                    list = list.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .child(
                                div()
                                    .w(px(22.))
                                    .text_size(px(18.))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(white.opacity(0.5))
                                    .child(format!("{}", i + 1)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(px(14.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(white)
                                            .child(song.title.clone()),
                                    )
                                    .when(!song.artist.is_empty(), |d| {
                                        d.child(div().truncate().text_size(px(12.)).text_color(soft).child(song.artist.clone()))
                                    }),
                            )
                            .child(line(trf!("{}×", "{}×", song.n), soft))
                            .with_animation(
                                ("song", i),
                                Animation::new(Duration::from_millis(380 + 110 * i as u64)).with_easing(gpui::ease_out_quint()),
                                |d, p| d.opacity(p).relative().left(px(-18. * (1. - p))),
                            ),
                    );
                }
                column()
                    .items_center()
                    .justify_center()
                    .gap(px(12.))
                    .child(kicker(tr!("Cirandinha", "Cirandinha"), soft))
                    .child(big_number(s.songs_played as i64, "", 64.))
                    .child(line(tr!("songs played for everyone", "músicas tocadas para todo mundo"), soft))
                    .child(list)
                    .when_some(s.top_dj.clone(), |d, dj| {
                        d.child(line(
                            trf!("The DJ of the year: {}, with {} songs", "O DJ do ano: {}, com {} músicas", dj.nickname, dj.n),
                            soft,
                        ))
                    })
                    .when_some(since(y.since.music), |d, day| d.child(fine(trf!("Counted since {}", "Contando desde {}", day), white)))
                    .into_any_element()
            }
            Slide::You => {
                let me = y.me.clone();
                let my_id = self.session.read(cx).me.id;
                let (my_name, face) = self.person(my_id, 72., cx);
                let partner = me.partner.clone().map(|p| (self.person(p.user_id, 20., cx).0, p.ms));
                let reaction = me.top_reaction.clone().map(|r| (self.emoji(&r.emoji, 22., cx), r.n));
                let mut tiles: Vec<(SharedString, AnyElement)> = Vec::new();
                let mut add = |title: SharedString, value: AnyElement| tiles.push((title, value));
                let strong =
                    |s: String| div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).text_color(white).child(s).into_any_element();
                add(
                    tr!("Your messages", "Suas mensagens").into(),
                    strong(match me.rank {
                        Some(r) => trf!("{} · #{} on the server", "{} · {}º do servidor", thousands(me.messages), r),
                        None => thousands(me.messages),
                    }),
                );
                if let Some(c) = me.top_channel.clone() {
                    add(tr!("Your place", "Seu lugar").into(), strong(format!("#{}", c.name)));
                }
                if me.voice_ms > 0 {
                    add(tr!("You in voice", "Você na voz").into(), strong(duration(me.voice_ms)));
                }
                if let Some((who, ms)) = partner {
                    add(tr!("Your duo", "Sua dupla").into(), strong(trf!("{} · {}", "{} · {}", who, duration(ms))));
                }
                if let Some(g) = me.top_game.clone() {
                    add(tr!("Your game", "Seu jogo").into(), strong(trf!("{} · {}", "{} · {}", g.name, duration(g.ms))));
                }
                if let Some((e, n)) = reaction {
                    add(
                        tr!("Your reaction", "Sua reação").into(),
                        div().flex().items_center().gap(px(8.)).child(e).child(strong(trf!("{}×", "{}×", n))).into_any_element(),
                    );
                }
                if me.night_messages > 0 {
                    add(
                        tr!("After midnight", "Depois da meia-noite").into(),
                        strong(trf!("{} messages", "{} mensagens", me.night_messages)),
                    );
                }
                let facts = div().flex().flex_wrap().justify_center().gap(px(10.)).w(px(560.)).children(tiles.into_iter().enumerate().map(
                    |(i, (title, value))| {
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .w(px(176.))
                            .px(px(14.))
                            .py(px(10.))
                            .rounded(px(radius::CARD))
                            .bg(white.opacity(0.08))
                            .child(div().text_size(px(11.5)).text_color(soft).child(title))
                            .child(value)
                            .with_animation(
                                ("you", i),
                                Animation::new(Duration::from_millis(380 + 90 * i as u64)).with_easing(gpui::ease_out_quint()),
                                |d, p| d.opacity(p).relative().top(px(12. * (1. - p))),
                            )
                    },
                ));
                column()
                    .items_center()
                    .justify_center()
                    .gap(px(12.))
                    .child(kicker(tr!("And you", "E você"), soft))
                    .child(div().flex().items_center().gap(px(14.)).child(face).child(headline(my_name, white)))
                    .child(facts)
                    .when_some(me.first_message.clone(), |d, f| {
                        d.child(div().max_w(px(520.)).text_size(px(12.5)).text_color(soft).child(trf!(
                            "Your first message of the year, on {}: “{}”",
                            "Sua primeira mensagem do ano, em {}: “{}”",
                            day_of_ms(f.at),
                            f.body
                        )))
                    })
                    .into_any_element()
            }
            Slide::Bye => column()
                .items_center()
                .justify_center()
                .gap(px(16.))
                .child(div().text_size(px(56.)).child("🎉"))
                .child(headline(trf!("Thanks for {}", "Valeu por {}", y.year), white))
                .child(line(tr!("Here's to the next one, together.", "Que venha o próximo, juntos."), soft))
                .into_any_element(),
        }
    }
}

impl Render for Retro {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = current();
        let white = gpui::white();
        let (body, count): (AnyElement, usize) = match &self.load {
            Load::Waiting => (
                column()
                    .items_center()
                    .justify_center()
                    .child(line(trf!("Counting {}…", "Contando {}…", self.year), white.opacity(0.7)))
                    .with_animation("retro-wait", Animation::new(Duration::from_millis(1400)).repeat(), |d, p| {
                        d.opacity(0.45 + 0.55 * (std::f32::consts::PI * p).sin())
                    })
                    .into_any_element(),
                0,
            ),
            Load::Failed(e) => (
                column()
                    .items_center()
                    .justify_center()
                    .gap(px(8.))
                    .child(headline(tr!("The year didn't load", "O ano não carregou"), white))
                    .child(line(e.clone(), white.opacity(0.7)))
                    .into_any_element(),
                0,
            ),
            Load::Ready(y) => {
                let y = (**y).clone();
                let slides = Self::slides(&y);
                self.at = self.at.min(slides.len() - 1);
                let which = slides[self.at];
                let el = self.slide(which, &y, cx);
                // Each slide comes in rising as it fades; its id is its place, so going back plays it again.
                (
                    div()
                        .size_full()
                        .child(el)
                        .with_animation(
                            ("retro-slide", self.at),
                            Animation::new(Duration::from_millis(420)).with_easing(gpui::ease_out_quint()),
                            |d, p| d.opacity(p).relative().top(px(18. * (1. - p))),
                        )
                        .into_any_element(),
                    slides.len(),
                )
            }
        };
        let at = self.at;
        let dots = div().flex().gap(px(5.)).children((0..count).map(|i| {
            div().h(px(4.)).rounded_full().bg(if i <= at { white.opacity(0.9) } else { white.opacity(0.25) }).w(px(if i == at {
                22.
            } else {
                10.
            }))
        }));
        let nav = |id: &'static str, glyph: &'static str, enabled: bool| {
            div()
                .id(id)
                .size(px(36.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(white.opacity(if enabled { 0.12 } else { 0.04 }))
                .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(gpui::white().opacity(0.22))))
                .child(icon(glyph, 16., white.opacity(if enabled { 0.95 } else { 0.3 })))
        };
        let (first, last) = (at == 0, count == 0 || at + 1 >= count);
        let accent = t.accent;
        div()
            .id("retro")
            .key_context("Retro")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .font_family(FONT)
            .relative()
            .w(px(W))
            .h(px(H))
            .max_w_full()
            .rounded(px(radius::PANE + 6.))
            .overflow_hidden()
            .border_1()
            .border_color(white.opacity(0.12))
            .shadow_lg()
            // A night sky in the accent: dark whatever the palette, so every slide reads the same.
            .bg(linear_gradient(145., linear_color_stop(mix_dark(accent, 0.3), 0.), linear_color_stop(gpui::hsla(0.75, 0.55, 0.1, 1.), 1.)))
            // Two soft glows, in the accent and in violet: light without edges.
            .child(glow(accent.opacity(0.42)).top(px(-70.)).right(px(-50.)))
            .child(glow(gpui::hsla(0.8, 0.7, 0.55, 0.3)).bottom(px(-90.)).left(px(-70.)))
            .child(div().absolute().inset_0().px(px(36.)).pt(px(44.)).pb(px(64.)).child(body))
            .child(div().absolute().top(px(18.)).left_0().right_0().flex().justify_center().child(dots))
            .child(
                div()
                    .absolute()
                    .bottom(px(16.))
                    .left(px(20.))
                    .right(px(20.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        nav("retro-back", "chevron-left", !first).when(!first, |d| d.on_click(cx.listener(|r, _, _, cx| r.step(-1, cx)))),
                    )
                    .child(
                        div()
                            .id("retro-close")
                            .px(px(12.))
                            .py(px(6.))
                            .rounded_full()
                            .cursor_pointer()
                            .text_size(px(12.))
                            .text_color(white.opacity(0.6))
                            .hover(|s| s.text_color(gpui::white()))
                            .child(tr!("Close", "Fechar"))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Dismiss))),
                    )
                    .child(if last {
                        nav("retro-next", "close", true).on_click(cx.listener(|_, _, _, cx| cx.emit(Dismiss)))
                    } else {
                        nav("retro-next", "chevron-right", true).on_click(cx.listener(|r, _, _, cx| r.step(1, cx)))
                    }),
            )
    }
}

// ------------------------------------------------------------------------------------- type

fn column() -> gpui::Div {
    div().size_full().flex().flex_col()
}

fn kicker(s: &str, color: Hsla) -> gpui::Div {
    div().font_family(crate::theme::MONO).text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(color).child(s.to_uppercase())
}

fn headline(s: impl Into<SharedString>, color: Hsla) -> gpui::Div {
    div().text_size(px(26.)).line_height(px(32.)).font_weight(FontWeight::BOLD).text_color(color).child(s.into())
}

fn line(s: impl Into<SharedString>, color: Hsla) -> gpui::Div {
    div().text_size(px(14.)).text_color(color).child(s.into())
}

fn fine(s: impl Into<SharedString>, color: Hsla) -> gpui::Div {
    div().text_size(px(11.)).text_color(color.opacity(0.45)).child(s.into())
}

/// A number counting up to `n` as it comes in.
fn big_number(n: i64, unit: &'static str, size: f32) -> AnyElement {
    counting(n, size, move |v| format!("{}{unit}", thousands(v)))
}

/// A year counting up, written as a year (2026, never 2.026).
fn big_year(year: i32, size: f32) -> AnyElement {
    counting(year as i64, size, |v| v.to_string())
}

fn counting(n: i64, size: f32, show: impl Fn(u64) -> String + 'static) -> AnyElement {
    div()
        .text_size(px(size))
        .line_height(px(size * 1.05))
        .font_weight(FontWeight::BOLD)
        .text_color(gpui::white())
        .with_animation(
            ("count", n as u64),
            Animation::new(Duration::from_millis(1100)).with_easing(gpui::ease_out_quint()),
            move |d, p| d.child(show((n as f32 * p).round() as u64)),
        )
        .into_any_element()
}

/// A round light with no edge, for behind the slides.
fn glow(color: Hsla) -> gpui::Div {
    div().absolute().size(px(200.)).rounded_full().bg(color.opacity(0.6)).shadow(vec![gpui::BoxShadow {
        color,
        offset: gpui::point(px(0.), px(0.)),
        blur_radius: px(150.),
        spread_radius: px(60.),
        inset: false,
    }])
}

fn game_colour(i: usize) -> Hsla {
    [
        gpui::hsla(0.95, 0.8, 0.62, 1.),
        gpui::hsla(0.55, 0.8, 0.6, 1.),
        gpui::hsla(0.13, 0.9, 0.6, 1.),
        gpui::hsla(0.38, 0.65, 0.55, 1.),
        gpui::hsla(0.75, 0.7, 0.68, 1.),
    ][i % 5]
}

fn mix_dark(c: Hsla, l: f32) -> Hsla {
    gpui::hsla(c.h, c.s.min(0.7), l.min(c.l), 1.)
}

// ------------------------------------------------------------------------------------- words

/// 12345 as "12.345" or "12,345".
fn thousands(n: u64) -> String {
    let sep = if crate::i18n::lang() == crate::i18n::Lang::Pt { '.' } else { ',' };
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(sep);
        }
        out.push(c);
    }
    out
}

/// "3 h 20 min", "45 min".
fn duration(ms: i64) -> String {
    let minutes = (ms / 60_000).max(0);
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) if h >= 10 => format!("{h} h {}", if m >= 30 { "½" } else { "" }).trim().to_string(),
        (h, m) => format!("{h} h {m} min"),
    }
}

const MONTHS_EN: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const MONTHS_PT: [&str; 12] =
    ["janeiro", "fevereiro", "março", "abril", "maio", "junho", "julho", "agosto", "setembro", "outubro", "novembro", "dezembro"];

/// "2026-10-08" as "October 8" or "8 de outubro".
fn long_date(date: &str) -> String {
    let mut parts = date.split('-').skip(1).filter_map(|p| p.parse::<usize>().ok());
    let (Some(month), Some(day)) = (parts.next(), parts.next()) else { return date.to_string() };
    let month = month.clamp(1, 12) - 1;
    match crate::i18n::lang() {
        crate::i18n::Lang::Pt => format!("{day} de {}", MONTHS_PT[month]),
        crate::i18n::Lang::En => format!("{} {day}", MONTHS_EN[month]),
    }
}

/// A moment (ms since the epoch) as its day in this computer's time.
fn day_of_ms(ms: i64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_millis_opt(ms).single() {
        Some(at) => long_date(&at.format("%Y-%m-%d").to_string()),
        None => String::new(),
    }
}

/// When `year` began here, in ms: a "since" earlier than this is the whole year and not said.
fn year_start(year: i32) -> i64 {
    use chrono::TimeZone;
    chrono::Local.with_ymd_and_hms(year, 1, 2, 0, 0, 0).single().map(|d| d.timestamp_millis()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_times_read_as_people_say_them() {
        crate::i18n::set(crate::i18n::Lang::Pt);
        assert_eq!(thousands(1234567), "1.234.567");
        assert_eq!(thousands(999), "999");
        assert_eq!(long_date("2026-10-08"), "8 de outubro");
        crate::i18n::set(crate::i18n::Lang::En);
        assert_eq!(thousands(1234), "1,234");
        assert_eq!(long_date("2026-03-01"), "March 1");
        assert_eq!(duration(45 * 60_000), "45 min");
        assert_eq!(duration(3 * 3_600_000 + 20 * 60_000), "3 h 20 min");
        assert_eq!(duration(2 * 3_600_000), "2 h");
        assert_eq!(duration(12 * 3_600_000 + 40 * 60_000), "12 h ½");
    }

    #[test]
    fn a_review_reads_whatever_the_server_leaves_out() {
        let y: Year = serde_json::from_value(serde_json::json!({ "year": 2026, "server": { "messages": 3 } })).unwrap();
        assert_eq!(y.server.messages, 3);
        let slides = Retro::slides(&y);
        assert!(slides.contains(&Slide::Messages));
        assert!(!slides.contains(&Slide::Voice));
        assert_eq!(slides.first(), Some(&Slide::Hello));
        assert_eq!(slides.last(), Some(&Slide::Bye));
    }
}
