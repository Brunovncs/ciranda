//! Cirandinha's screens: the strip in the call's card that says what is playing, and the panel
//! its button opens, with the search, the controls and the queue. Everybody in the voice channel
//! hears the same music, and everybody in it may add to it and control it.
//!
//! Nothing here plays anything (that is `media::music`): a press is a request to the server, and
//! what is drawn is the state the server last sent, with the position worked out from its clock.

use super::{VolumeSlider, request};
use crate::core::{self, api::ApiError, types::*};
use crate::media::music::MusicPlayer;
use crate::prefs::{prefs, set_prefs};
use crate::session::{Picture, Session};
use crate::text_field::{TextField, TextFieldEvent};
use crate::theme::{Theme, current, px, radius};
use crate::ui::overlay::Dismiss;
use crate::widgets::*;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Bounds, Context, Entity, EventEmitter, Focusable, Hsla, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, Pixels, Render, SharedString, StatefulInteractiveElement, Styled, StyledImage, Subscription, Task,
    Window, canvas, div, img,
};
use serde_json::json;
use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

/// The panel's height where the window has room for it.
const PANEL_HEIGHT: f32 = 640.;

/// How often a playing track's time is drawn again.
const REDRAW: Duration = Duration::from_millis(500);

/// `m:ss`, or `h:mm:ss` past the hour.
fn clock(ms: f64) -> String {
    let s = (ms.max(0.) / 1000.) as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

fn untitled(title: &str) -> SharedString {
    if title.trim().is_empty() { tr!("Untitled", "Sem título").into() } else { title.to_string().into() }
}

/// A track's picture, or a note where there is none.
fn cover(session: &Entity<Session>, hash: Option<&str>, w: f32, h: f32, t: &Theme, cx: &mut App) -> AnyElement {
    let picture = hash.map(|h| session.update(cx, |s, cx| s.picture(h, cx)));
    let frame = div().flex_none().w(px(w)).h(px(h)).rounded(px(radius::INNER)).overflow_hidden().bg(t.well);
    match picture {
        Some(Picture::Ready(image)) => frame.child(img(image).w(px(w)).h(px(h)).object_fit(gpui::ObjectFit::Cover)).into_any_element(),
        _ => frame.flex().items_center().justify_center().child(icon("music", (h * 0.5).min(18.), t.text3)).into_any_element(),
    }
}

/// Says the track ought to be drawn again shortly while the music plays, so its time moves.
fn keep_drawing<V: 'static>(cx: &mut Context<V>) -> Task<()> {
    cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor().timer(REDRAW).await;
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                return;
            }
        }
    })
}

/// Sends one of the controls for the channel's music.
fn control(session: &Entity<Session>, channel: ChannelId, action: &str, mut args: serde_json::Value, cx: &mut App) {
    args["channelId"] = json!(channel);
    args["action"] = json!(action);
    request(session, "music:control", args, cx);
}

/// What the music is, for drawing: the server's state and where in the track it ought to be.
struct Now {
    state: MusicState,
    position: f64,
    fetching: bool,
}

fn now(session: &Entity<Session>, player: &Entity<MusicPlayer>, channel: ChannelId, cx: &App) -> Option<Now> {
    let live = session.read(cx).music.as_ref().filter(|m| m.channel == channel)?;
    let p = player.read(cx);
    Some(Now { state: live.state.clone(), position: p.position_ms(cx), fetching: p.fetching(cx) })
}

/// A thin bar of how far the track has got; a click on it goes there.
fn seek_bar(id: &'static str, fraction: f32, height: f32, t: &Theme, on_seek: Option<impl Fn(f32, &mut App) + 'static>) -> AnyElement {
    let bounds: Rc<Cell<Bounds<Pixels>>> = Rc::new(Cell::new(Bounds::default()));
    let seen = bounds.clone();
    let accent = t.accent;
    let mut bar = div()
        .id(id)
        .relative()
        .w_full()
        .h(px(height + 8.))
        .flex()
        .items_center()
        .child(canvas(move |b, _, _| seen.set(b), |_, _, _, _| {}).absolute().inset_0())
        .child(div().relative().w_full().h(px(height)).rounded(px(height / 2.)).bg(t.well).child(
            div().absolute().left_0().top_0().h_full().rounded(px(height / 2.)).bg(accent).w(gpui::relative(fraction.clamp(0., 1.))),
        ));
    if let Some(on_seek) = on_seek {
        bar = bar.cursor_pointer().on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, _, cx| {
            cx.stop_propagation();
            let b = bounds.get();
            let w = f32::from(b.size.width);
            if w > 0. {
                on_seek((f32::from(e.position.x - b.origin.x) / w).clamp(0., 1.), cx);
            }
        });
    }
    bar.into_any_element()
}

fn small_button(id: impl Into<gpui::ElementId>, glyph: &'static str, color: Hsla, t: &Theme) -> gpui::Stateful<gpui::Div> {
    let hover = t.control_hover;
    div()
        .id(id.into())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(26.))
        .rounded(px(6.))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .active(|s| s.opacity(0.75))
        .child(icon(glyph, 14., color))
}

// ------------------------------------------------------------------------------------------ strip

/// What is playing, in the call's card: the track, play or pause, next, and how far it has got.
/// A click on the track opens the panel.
pub struct MusicStrip {
    session: Entity<Session>,
    player: Entity<MusicPlayer>,
    channel: ChannelId,
    _tick: Task<()>,
    _subs: Vec<Subscription>,
}

impl MusicStrip {
    pub fn new(session: Entity<Session>, player: Entity<MusicPlayer>, channel: ChannelId, cx: &mut Context<Self>) -> MusicStrip {
        let subs = vec![cx.observe(&session, |_, _, cx| cx.notify()), cx.observe(&player, |_, _, cx| cx.notify())];
        MusicStrip { session, player, channel, _tick: keep_drawing(cx), _subs: subs }
    }

    pub fn player(&self) -> &Entity<MusicPlayer> {
        &self.player
    }
}

impl Render for MusicStrip {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = current();
        let Some(n) = now(&self.session, &self.player, self.channel, cx) else { return div().into_any_element() };
        let Some(track) = n.state.current_track().cloned() else { return div().into_any_element() };
        let (session, channel) = (self.session.clone(), self.channel);
        let playing = n.state.status == MusicStatus::Playing;
        let fraction = track.duration_ms.map_or(0., |d| (n.position / d.max(1) as f64) as f32);
        let subtitle: SharedString = if n.state.status == MusicStatus::Loading || n.fetching {
            tr!("Getting it ready…", "Preparando…").into()
        } else if track.artist.is_empty() {
            clock(n.position).into()
        } else {
            track.artist.clone().into()
        };
        let (s1, s2) = (session.clone(), session.clone());
        let player = self.player.clone();
        let _ = window;
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(cover(&self.session, track.thumb_hash.as_deref(), 32., 32., &t, cx))
                    .child(
                        div()
                            .id("music-strip-track")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            .cursor_pointer()
                            .child(div().truncate().text_size(px(12.)).text_color(t.text).child(untitled(&track.title)))
                            .child(div().truncate().text_size(px(11.)).text_color(t.text3).child(subtitle))
                            .tooltip(tip(tr!("Open Cirandinha", "Abrir a Cirandinha"), &t))
                            .on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                open(session.clone(), player.clone(), channel, e.position, window, cx);
                            }),
                    )
                    .child(
                        small_button("music-strip-play", if playing { "pause" } else { "play" }, t.text, &t)
                            .tooltip(tip(if playing { tr!("Pause", "Pausar") } else { tr!("Play", "Tocar") }, &t))
                            .on_click(move |_, _, cx| control(&s1, channel, if playing { "pause" } else { "resume" }, json!({}), cx)),
                    )
                    .child(
                        small_button("music-strip-next", "skip-forward", t.text, &t)
                            .tooltip(tip(tr!("Next", "Próxima"), &t))
                            .on_click(move |_, _, cx| control(&s2, channel, "next", json!({}), cx)),
                    ),
            )
            .child(seek_bar("music-strip-progress", fraction, 2., &t, None::<fn(f32, &mut App)>))
            .into_any_element()
    }
}

// ------------------------------------------------------------------------------------------ panel

/// Opens the panel over the call's card.
pub fn open(
    session: Entity<Session>,
    player: Entity<MusicPlayer>,
    channel: ChannelId,
    at: gpui::Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let view = cx.new(|cx| MusicPanel::new(session, player, channel, cx));
    view.read(cx).search.focus_handle(cx).focus(window, cx);
    crate::ui::overlay::open_menu_above(view, at, cx);
}

/// What a search found.
struct Found {
    resolved: MusicResolved,
    /// Sources already sent to the queue from these results.
    added: HashSet<String>,
}

pub struct MusicPanel {
    session: Entity<Session>,
    player: Entity<MusicPlayer>,
    channel: ChannelId,
    search: Entity<TextField>,
    found: Option<Found>,
    /// A search is on its way.
    busy: bool,
    error: Option<SharedString>,
    _tick: Task<()>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<Dismiss> for MusicPanel {}

impl MusicPanel {
    fn new(session: Entity<Session>, player: Entity<MusicPlayer>, channel: ChannelId, cx: &mut Context<Self>) -> MusicPanel {
        let search = cx.new(|cx| {
            TextField::new(cx, false, 300).enter_submits().placeholder(tr!(
                "A YouTube or Spotify link, or a song's name",
                "Um link do YouTube ou do Spotify, ou o nome de uma música"
            ))
        });
        let subs = vec![
            cx.observe(&session, |_, _, cx| cx.notify()),
            cx.observe(&player, |_, _, cx| cx.notify()),
            cx.subscribe(&search, |this, _, ev: &TextFieldEvent, cx| {
                if let TextFieldEvent::Submit = ev {
                    this.find(cx);
                }
            }),
        ];
        MusicPanel { session, player, channel, search, found: None, busy: false, error: None, _tick: keep_drawing(cx), _subs: subs }
    }

    /// Asks the server what the box names.
    fn find(&mut self, cx: &mut Context<Self>) {
        let input = self.search.read(cx).text().trim().to_string();
        self.look_up(input, cx);
    }

    fn look_up(&mut self, input: String, cx: &mut Context<Self>) {
        if input.is_empty() || self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let api = self.session.read(cx).api.clone();
        cx.spawn(async move |this, cx| {
            let got = core::run(async move { api.music_resolve(&input).await }).await;
            let _ = this.update(cx, |p, cx| {
                p.busy = false;
                match got {
                    Ok(r) if r.items.is_empty() => {
                        p.found = None;
                        p.error = Some(tr!("Nothing found.", "Nada encontrado.").into());
                    }
                    Ok(r) => p.found = Some(Found { resolved: r, added: HashSet::new() }),
                    Err(e) => p.error = Some(resolve_error(&e).into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn add(&mut self, items: Vec<MusicFound>, next: bool, cx: &mut Context<Self>) {
        if items.is_empty() {
            return;
        }
        if let Some(f) = &mut self.found {
            f.added.extend(items.iter().map(|i| i.source.clone()));
        }
        request(&self.session, "music:add", json!({ "channelId": self.channel, "items": items, "next": next }), cx);
        cx.notify();
    }

    fn render_results(&mut self, found: &Found, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let items = found.resolved.items.clone();
        let count = items.len();
        let many = count > 1 && found.resolved.kind != "search";
        let playlist = found.resolved.playlist.clone();
        let mut list = div().id("music-results").flex().flex_col().gap(px(2.)).max_h(px(220.)).overflow_y_scroll();
        for (i, item) in items.iter().enumerate() {
            let added = found.added.contains(&item.source);
            let (one, next) = (item.clone(), item.clone());
            let hover = t.layer_hover;
            list = list.child(
                div()
                    .id(("music-found", i))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .p(px(4.))
                    .rounded(px(radius::INNER))
                    .hover(move |s| s.bg(hover))
                    .child(cover(&self.session, item.thumb_hash.as_deref(), 48., 27., t, cx))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            .child(div().truncate().text_size(px(12.)).text_color(t.text).child(untitled(&item.title)))
                            .child(div().truncate().text_size(px(11.)).text_color(t.text3).child(item.artist.clone())),
                    )
                    .when_some(item.duration_ms, |d, ms| d.child(mono(clock(ms as f64), t.text3)))
                    .map(|d| {
                        if added {
                            d.child(
                                div().flex_none().size(px(26.)).flex().items_center().justify_center().child(icon("check", 14., t.success)),
                            )
                        } else {
                            d.child(
                                small_button(("music-found-next", i), "play-next", t.text2, t)
                                    .tooltip(tip(tr!("Play next", "Tocar a seguir"), t))
                                    .on_click(cx.listener(move |this, _, _, cx| this.add(vec![next.clone()], true, cx))),
                            )
                            .child(
                                small_button(("music-found-add", i), "plus", t.text2, t)
                                    .tooltip(tip(tr!("Add to the queue", "Adicionar à fila"), t))
                                    .on_click(cx.listener(move |this, _, _, cx| this.add(vec![one.clone()], false, cx))),
                            )
                        }
                    }),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(div().flex_1().child(caption(
                        if count == 1 { tr!("1 result", "1 resultado").to_string() } else { trf!("{} results", "{} resultados", count) },
                        t.text2,
                    )))
                    .when(many, |d| {
                        let all = items.clone();
                        d.child(
                            button("music-add-all", trf!("Add all {}", "Adicionar as {}", count), Kind::Standard, t)
                                .h(px(28.))
                                .on_click(cx.listener(move |this, _, _, cx| this.add(all.clone(), false, cx))),
                        )
                    })
                    .when_some(playlist, |d, list| {
                        d.child(
                            button("music-whole-playlist", tr!("The whole playlist", "A playlist toda"), Kind::Standard, t)
                                .h(px(28.))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.look_up(format!("https://www.youtube.com/playlist?list={list}"), cx)
                                })),
                        )
                    })
                    .child(
                        small_button("music-results-close", "close", t.text3, t)
                            .tooltip(tip(tr!("Close the results", "Fechar os resultados"), t))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.found = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(list)
            .into_any_element()
    }

    fn render_now(&mut self, n: &Now, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let (session, channel) = (self.session.clone(), self.channel);
        let state = &n.state;
        let track = state.current_track().cloned();
        let playing = state.status == MusicStatus::Playing;
        let duration = track.as_ref().and_then(|t| t.duration_ms);
        let fraction = duration.map_or(0., |d| (n.position / d.max(1) as f64) as f32);
        let seek = {
            let s = session.clone();
            duration.map(|d| {
                move |f: f32, cx: &mut App| control(&s, channel, "seek", json!({ "positionMs": (f as f64 * d as f64) as u64 }), cx)
            })
        };
        let c = |action: &'static str, args: serde_json::Value| {
            let s = session.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| control(&s, channel, action, args.clone(), cx)
        };
        let (shuffle, repeat) = (state.shuffle, state.repeat);
        let header = match &track {
            Some(tr) => {
                let line = match (&tr.added_by, state.status == MusicStatus::Loading || n.fetching) {
                    (_, true) => tr!("Getting it ready…", "Preparando…").to_string(),
                    (Some(by), _) if !tr.artist.is_empty() => trf!("{} · added by {}", "{} · adicionada por {}", tr.artist, by),
                    (Some(by), _) => trf!("added by {}", "adicionada por {}", by),
                    (None, _) => tr.artist.clone(),
                };
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(cover(&self.session, tr.thumb_hash.as_deref(), 64., 36., t, cx))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(13.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(t.text)
                                    .child(untitled(&tr.title)),
                            )
                            .child(div().truncate().text_size(px(11.)).text_color(t.text3).child(line)),
                    )
                    .into_any_element()
            }
            None => caption(
                tr!("Nothing is playing. Look for a song or paste a link.", "Nada está tocando. Procure uma música ou cole um link."),
                t.text3,
            )
            .into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(header)
            .when(track.is_some(), |d| {
                d.child(seek_bar("music-progress", fraction, 4., t, seek)).child(
                    div()
                        .flex()
                        .justify_between()
                        .child(mono(clock(n.position), t.text3))
                        .child(mono(duration.map(|d| clock(d as f64)).unwrap_or_default(), t.text3)),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(6.))
                    .child(
                        tool_button("music-shuffle", "shuffle", shuffle, if shuffle { t.accent } else { t.text2 }, t)
                            .tooltip(tip(tr!("Shuffle", "Embaralhar"), t))
                            .on_click(c("shuffle", json!({ "on": !shuffle }))),
                    )
                    .child(
                        tool_button("music-prev", "skip-back", false, t.text, t)
                            .tooltip(tip(tr!("Previous", "Anterior"), t))
                            .on_click(c("prev", json!({}))),
                    )
                    .child(
                        div()
                            .id("music-play")
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(px(40.))
                            .rounded_full()
                            .bg(t.accent)
                            .cursor_pointer()
                            .active(|s| s.opacity(0.8))
                            .child(icon(if playing { "pause" } else { "play" }, 18., t.on_accent))
                            .tooltip(tip(if playing { tr!("Pause", "Pausar") } else { tr!("Play", "Tocar") }, t))
                            .on_click(c(if playing { "pause" } else { "resume" }, json!({}))),
                    )
                    .child(
                        tool_button("music-next", "skip-forward", false, t.text, t)
                            .tooltip(tip(tr!("Next", "Próxima"), t))
                            .on_click(c("next", json!({}))),
                    )
                    .child(
                        tool_button(
                            "music-repeat",
                            if repeat == Repeat::One { "repeat-one" } else { "repeat" },
                            repeat != Repeat::Off,
                            if repeat == Repeat::Off { t.text2 } else { t.accent },
                            t,
                        )
                        .tooltip(tip(
                            match repeat {
                                Repeat::Off => tr!("Repeat: off", "Repetir: desligado"),
                                Repeat::All => tr!("Repeat: the whole queue", "Repetir: a fila toda"),
                                Repeat::One => tr!("Repeat: this song", "Repetir: esta música"),
                            },
                            t,
                        ))
                        .on_click(c("repeat", json!({ "mode": repeat.next().wire() }))),
                    ),
            )
            .into_any_element()
    }

    fn render_queue(&mut self, n: &Now, t: &Theme) -> AnyElement {
        let (session, channel) = (self.session.clone(), self.channel);
        let queue = n.state.queue.clone();
        let len = queue.len();
        let current = n.state.current.clone();
        let mut list = div().id("music-queue").flex().flex_col().flex_1().min_h(px(0.)).gap(px(1.)).overflow_y_scroll();
        for (i, track) in queue.iter().enumerate() {
            let is_current = current.as_deref() == Some(track.id.as_str());
            let failed = track.error.is_some();
            let id = track.id.clone();
            let hover = t.layer_hover;
            let group: SharedString = format!("music-row-{i}").into();
            let s = session.clone();
            let act = move |action: &'static str, args: serde_json::Value| {
                let s = s.clone();
                move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
                    cx.stop_propagation();
                    control(&s, channel, action, args.clone(), cx)
                }
            };
            let play = act("play", json!({ "id": id }));
            list = list.child(
                div()
                    .id(("music-row", i))
                    .group(group.clone())
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(6.))
                    .py(px(4.))
                    .rounded(px(radius::INNER))
                    .cursor_pointer()
                    .when(is_current, |d| d.bg(t.accent_soft))
                    .when(!is_current, |d| d.hover(move |s| s.bg(hover)))
                    .on_click(play)
                    .child(div().w(px(18.)).flex_none().flex().justify_center().map(|d| {
                        if is_current { d.child(icon("music", 13., t.accent)) } else { d.child(mono(format!("{}", i + 1), t.text3)) }
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(12.))
                                    .text_color(if failed {
                                        t.text3
                                    } else if is_current {
                                        t.accent
                                    } else {
                                        t.text
                                    })
                                    .when(failed, |d| d.line_through())
                                    .child(untitled(&track.title)),
                            )
                            .child(div().truncate().text_size(px(11.)).text_color(t.text3).child(match (&track.error, &track.added_by) {
                                (Some(_), _) => tr!("could not be played", "não foi possível tocar").to_string(),
                                (None, Some(by)) if !track.artist.is_empty() => format!("{} · {by}", track.artist),
                                (None, Some(by)) => by.clone(),
                                (None, None) => track.artist.clone(),
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(1.))
                            .invisible()
                            .group_hover(group.clone(), |s| s.visible())
                            .when(!is_current, |d| {
                                d.child(
                                    small_button(("music-row-next", i), "play-next", t.text2, t)
                                        .tooltip(tip(tr!("Play next", "Tocar a seguir"), t))
                                        .on_click(act("next-up", json!({ "id": track.id }))),
                                )
                            })
                            .when(i > 0, |d| {
                                d.child(
                                    small_button(("music-row-up", i), "chevron-up", t.text2, t)
                                        .tooltip(tip(tr!("Move up", "Subir"), t))
                                        .on_click(act("move", json!({ "id": track.id, "index": i - 1 }))),
                                )
                            })
                            .when(i + 1 < len, |d| {
                                d.child(
                                    small_button(("music-row-down", i), "chevron-down", t.text2, t)
                                        .tooltip(tip(tr!("Move down", "Descer"), t))
                                        .on_click(act("move", json!({ "id": track.id, "index": i + 1 }))),
                                )
                            })
                            .child(
                                small_button(("music-row-remove", i), "close", t.text2, t)
                                    .tooltip(tip(tr!("Remove from the queue", "Remover da fila"), t))
                                    .on_click(act("remove", json!({ "id": track.id }))),
                            ),
                    )
                    .when_some(track.duration_ms, |d, ms| d.child(div().flex_none().child(mono(clock(ms as f64), t.text3)))),
            );
        }
        let (s1, s2) = (session.clone(), session.clone());
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .gap(px(6.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(div().flex_1().child(label(
                        if len == 1 {
                            tr!("1 song in the queue", "1 música na fila").to_string()
                        } else {
                            trf!("{} songs in the queue", "{} músicas na fila", len)
                        },
                        t,
                    )))
                    .child(
                        small_button("music-mix", "sparkles", t.text2, t)
                            .tooltip(tip(tr!("Shuffle what is left, again", "Embaralhar o que falta de novo"), t))
                            .on_click(move |_, _, cx| control(&s1, channel, "reshuffle", json!({}), cx)),
                    )
                    .child(
                        small_button("music-clear", "delete", t.critical, t)
                            .tooltip(tip(tr!("Clear the queue for everyone", "Limpar a fila para todos"), t))
                            .on_click(move |_, _, cx| control(&s2, channel, "clear", json!({}), cx)),
                    ),
            )
            .child(list)
            .into_any_element()
    }
}

/// What to say when a search is turned down.
fn resolve_error(e: &ApiError) -> String {
    match e.code {
        // The server's "too_long" is a track past its limit, not a name.
        ErrorCode::TooLong => tr!("That is too long to play here.", "Isso é longo demais para tocar aqui.").into(),
        ErrorCode::Timeout if e.status == 504 => tr!("YouTube took too long to answer.", "O YouTube demorou demais para responder.").into(),
        _ if e.body.get("error").and_then(|v| v.as_str()) == Some("empty") => {
            tr!("Type a link or some words.", "Digite um link ou algumas palavras.").into()
        }
        _ => e.message.clone(),
    }
}

impl Render for MusicPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = current();
        // A height of its own, the queue scrolling inside it: grown by every song added, the panel
        // (which opens upward) would move what is under the pointer between two clicks.
        let height = (f32::from(window.viewport_size().height) / crate::theme::scale() - 120.).clamp(320., PANEL_HEIGHT);
        let n = now(&self.session, &self.player, self.channel, cx);
        let p = prefs(cx);
        let (volume, muted) = (p.music_volume as f32 / 100., p.music_muted);
        let found = self.found.take();
        let results = found.as_ref().map(|f| self.render_results(f, &t, cx));
        self.found = found;
        let empty = Now { state: MusicState::default(), position: 0., fetching: false };
        let now_el = self.render_now(n.as_ref().unwrap_or(&empty), &t, cx);
        let queue = n.as_ref().filter(|n| !n.state.queue.is_empty()).map(|n| self.render_queue(n, &t));
        let busy = self.busy;
        div()
            .w(px(440.))
            .h(px(height))
            .flex()
            .flex_col()
            .gap(px(12.))
            .p(px(14.))
            .rounded(px(radius::CONTROL + 2.))
            .bg(t.popover)
            .border_1()
            .border_color(t.stroke_strong)
            .shadow_lg()
            .occlude()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(icon("music", 18., t.accent))
                    .child(div().flex_1().child(title("Cirandinha", t.text)))
                    .child(mono(
                        format!("{:.0}%", volume * 100.),
                        if muted {
                            t.text3
                        } else if volume > 1. {
                            t.caution
                        } else {
                            t.text3
                        },
                    ))
                    .child(
                        tool_button(
                            "music-mute",
                            if muted || volume == 0. { "volume-off" } else { "volume" },
                            muted,
                            if muted { t.critical } else { t.text2 },
                            &t,
                        )
                        .tooltip(tip(
                            if muted {
                                tr!("Unmute the music", "Reativar a música")
                            } else {
                                tr!("Mute the music", "Silenciar a música")
                            },
                            &t,
                        ))
                        .on_click(|_, _, cx| set_prefs(cx, |p| p.music_muted = !p.music_muted)),
                    ),
            )
            .child(
                VolumeSlider::new("music-volume", volume, |g, cx| {
                    set_prefs(cx, |p| {
                        p.music_volume = (g * 100.).round() as u32;
                        // Moving it is a way of un-muting it, as it is for a person.
                        p.music_muted = false;
                    })
                })
                .width(px(412.))
                .render(&t, cx),
            )
            .child(
                div().flex().items_center().gap(px(8.)).child(div().flex_1().child(self.search.clone())).child(
                    icon_label_button(
                        "music-find",
                        "search",
                        if busy { tr!("Looking…", "Buscando…") } else { tr!("Search", "Buscar") },
                        Kind::Primary,
                        &t,
                    )
                    .when(busy, |d| d.opacity(0.6))
                    .on_click(cx.listener(|this, _, _, cx| this.find(cx))),
                ),
            )
            .when_some(self.error.clone(), |d, e| d.child(caption(e, t.critical)))
            .children(results)
            .child(div().h(px(1.)).bg(t.stroke))
            .child(now_el)
            .children(queue)
    }
}

/// The owner's switch, in the server's settings.
pub fn owner_card(session: &Entity<Session>, info: Option<&ServerInfo>, owner: bool, t: &Theme) -> Option<AnyElement> {
    let info = info?;
    // Only from a server that knows about it.
    let available = info.music_available?;
    let on = info.music_enabled;
    let mut row = div()
        .id("music-enabled")
        .flex()
        .items_center()
        .gap(px(10.))
        .child(icon("music", 16., t.text2))
        .child(div().flex_1().child(body(tr!("Cirandinha: music in voice channels", "Cirandinha: música nos canais de voz"), t.text)))
        .child(switch(on, t).when(!owner || (!available && !on), |d| d.opacity(0.5)));
    if owner && (available || on) {
        let session = session.clone();
        row = row.cursor_pointer().on_click(move |_, _, cx| {
            session.update(cx, |s, cx| {
                s.call(cx, move |api| Box::pin(async move { api.set_music_enabled(!on).await }), |s, info, cx| s.set_server_info(info, cx))
            })
        });
    }
    let mut card = card(t).child(row).child(caption(
        tr!(
            "Anyone in a voice channel can put on music for it, from YouTube and Spotify links or a search. The server fetches the audio with yt-dlp; everyone hears it at the same moment.",
            "Quem estiver num canal de voz pode colocar música para todos, com links do YouTube e do Spotify ou uma busca. O servidor baixa o áudio com o yt-dlp, e todos ouvem no mesmo momento."
        ),
        t.text2,
    ));
    if !available {
        card = card.child(caption(
            tr!(
                "yt-dlp is not installed on this server, so this cannot be turned on.",
                "O yt-dlp não está instalado neste servidor, então isto não pode ser ligado."
            ),
            t.caution,
        ));
    }
    if !owner {
        card = card.child(caption(tr!("Only the owner can change it.", "Só o dono pode mudar isso."), t.text3));
    }
    Some(card.into_any_element())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_read_as_a_player_shows_them() {
        assert_eq!(clock(0.), "0:00");
        assert_eq!(clock(61_999.), "1:01");
        assert_eq!(clock(3_725_000.), "1:02:05");
        assert_eq!(clock(-5.), "0:00");
    }
}
