//! "What's new": the dialog that greets the first start of a new version with what changed since
//! the one last seen, every skipped version included, newest first; and the same dialog with the
//! whole history, from Settings. The notes themselves are `crate::news`.

use super::Root;
use crate::core::update::is_newer;
use crate::news::{self, Entry, Kind as NoteKind};
use crate::prefs::{prefs, set_prefs};
use crate::theme::{Theme, current, px};
use crate::ui::overlay::{self, Dismiss, dialog_card};
use crate::widgets::*;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyWindowHandle, App, AppContext, Context, EventEmitter, FocusHandle, Focusable, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, StatefulInteractiveElement, Styled, Window, div,
};
use std::time::Duration;

const RELEASES: &str = "https://github.com/Brunovncs/ciranda/releases";
/// How often the start looks again for the window to be ready for the dialog.
const POLL: Duration = Duration::from_millis(400);
/// The longest it waits for a kept sign-in to go in before showing it anyway.
const WAIT_TICKS: u32 = 50;

/// The version last shown and the entries since, when this start has news to show.
fn pending(cx: &App) -> Option<(String, Vec<&'static Entry>)> {
    let p = prefs(cx);
    let seen = news::seen(&p.news_seen, !p.saved_servers.is_empty())?;
    let entries = news::since(seen, news::current());
    (!entries.is_empty()).then(|| (seen.to_string(), entries))
}

/// Whether this start opens the dialog, which says the version itself.
pub fn will_greet(cx: &App) -> bool {
    pending(cx).is_some()
}

/// This version's news counts as shown. Never back: a version put back after an update would
/// otherwise show its successor's news again once it returns.
fn mark_seen(cx: &mut App) {
    let now = news::current();
    set_prefs(cx, |p| {
        if p.news_seen.trim().is_empty() || is_newer(now, &p.news_seen) {
            p.news_seen = now.to_string();
        }
    });
}

/// At start: the dialog, once the window has settled, when there is news since the version last
/// shown. Otherwise this version is noted as seen, so a new install starts counting from it.
pub fn greet(window: AnyWindowHandle, cx: &mut Context<Root>) {
    if pending(cx).is_none() {
        cx.defer(mark_seen);
        return;
    }
    cx.spawn(async move |root, cx| {
        // Not over the loading screen while a kept sign-in goes in: it would jump as the server
        // replaces it.
        for _ in 0..WAIT_TICKS {
            cx.background_executor().timer(POLL).await;
            match root.update(cx, |root, cx| root.resuming(cx)) {
                Ok(true) => continue,
                Ok(false) => break,
                Err(_) => return,
            }
        }
        let _ = window.update(cx, |_, window, cx| {
            if let Some((seen, entries)) = pending(cx) {
                open(entries, Some(seen), window, cx);
            }
        });
    })
    .detach();
}

/// Every version's news, from Settings.
pub fn open_history(window: &mut Window, cx: &mut App) {
    open(news::up_to(news::current()), None, window, cx);
}

fn open(entries: Vec<&'static Entry>, since: Option<String>, window: &mut Window, cx: &mut App) {
    let greeting = since.is_some();
    let view = cx.new(|cx| News { focus: cx.focus_handle(), entries, since });
    // Once it is gone, however it closed. Escape and a click beside it drop the view without a
    // `Dismiss` reaching anyone, so its release is what is watched.
    if greeting {
        cx.observe_release(&view, |_, cx| mark_seen(cx)).detach();
    }
    overlay::open_dialog(view, window, cx);
}

pub struct News {
    focus: FocusHandle,
    entries: Vec<&'static Entry>,
    /// The version last shown, when greeting an update; none for the whole history.
    since: Option<String>,
}

impl EventEmitter<Dismiss> for News {}

impl Focusable for News {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn kind_label(kind: NoteKind) -> &'static str {
    match kind {
        NoteKind::New => tr!("New", "Novo"),
        NoteKind::Better => tr!("Improved", "Melhorias"),
        NoteKind::Fixed => tr!("Fixed", "Correções"),
    }
}

fn kind_look(kind: NoteKind, t: &Theme) -> (&'static str, gpui::Hsla) {
    match kind {
        NoteKind::New => ("sparkles", t.accent_text),
        NoteKind::Better => ("chevron-up", t.text2),
        NoteKind::Fixed => ("check", t.success),
    }
}

fn release(e: &Entry, t: &Theme) -> gpui::Div {
    let date = e.date().map(|d| crate::i18n::date(&d)).unwrap_or_default();
    let mut section = div().flex().flex_col().gap(px(14.)).child(
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(title(e.version, t.text))
            .when(e.version == news::current(), |d| d.child(chip(tr!("This version", "Esta versão"), t.accent_text)))
            .child(div().flex_1())
            .child(caption(date, t.text3)),
    );
    for kind in NoteKind::ALL {
        let notes: Vec<_> = e.notes.iter().filter(|n| n.kind == kind).collect();
        if notes.is_empty() {
            continue;
        }
        let (glyph, color) = kind_look(kind, t);
        section = section.child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(div().flex().items_center().gap(px(6.)).child(icon(glyph, 13., color)).child(label(kind_label(kind), t)))
                .children(notes.into_iter().map(|n| {
                    div()
                        .flex()
                        .gap(px(10.))
                        .pl(px(3.))
                        .child(div().flex_none().mt(px(8.)).size(px(4.)).rounded_full().bg(t.text3))
                        .child(div().flex_1().min_w(px(0.)).child(body(n.text(), t.text)))
                })),
        );
    }
    section
}

impl Render for News {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = current();
        let now = news::current();
        let greeting = self.since.is_some();
        let subtitle = match &self.since {
            Some(_) if self.entries.len() == 1 => trf!("Ciranda is now on version {}.", "Ciranda agora está na versão {}.", now),
            Some(seen) => trf!(
                "Ciranda is now on version {}. Here is what changed since {}, the newest first.",
                "Ciranda agora está na versão {}. Veja o que mudou desde a {}, da mais nova para a mais antiga.",
                now,
                seen
            ),
            None => tr!("What changed in each version, the newest first.", "O que mudou em cada versão, da mais nova para a mais antiga.")
                .to_string(),
        };
        let max_h = (window.viewport_size().height * 0.85).min(px(680.));
        let last = self.entries.len().saturating_sub(1);
        let mut list = div().id("news-list").flex().flex_col().min_h(px(0.)).overflow_y_scroll().px(px(24.)).pt(px(4.)).pb(px(22.));
        for (i, e) in self.entries.iter().enumerate() {
            list = list.child(release(e, &t)).when(i < last, |d| d.child(div().my(px(20.)).h(px(1.)).bg(t.stroke)));
        }
        dialog_card(&t, 540.)
            .key_context("News")
            .track_focus(&self.focus)
            .max_h(max_h)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_start()
                    .gap(px(14.))
                    .px(px(24.))
                    .pt(px(22.))
                    .pb(px(18.))
                    .child(super::brand_mark(40., &t))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap(px(3.))
                            .child(div().text_size(px(20.)).line_height(px(26.)).font_weight(FontWeight::BOLD).text_color(t.text).child(
                                if greeting { tr!("What's new", "Novidades") } else { tr!("Version history", "Histórico de versões") },
                            ))
                            .child(caption(subtitle, t.text2)),
                    )
                    .child(icon_button("news-close", "close", &t).on_click(cx.listener(|_, _, _, cx| cx.emit(Dismiss)))),
            )
            .child(list)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(24.))
                    .py(px(14.))
                    .bg(t.layer)
                    .border_t_1()
                    .border_color(t.stroke)
                    .when(!greeting, |d| {
                        d.child(
                            button("news-github", tr!("Older versions on GitHub", "Versões antigas no GitHub"), Kind::Subtle, &t)
                                .on_click(|_, _, cx| cx.open_url(RELEASES)),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        button("news-ok", if greeting { tr!("Got it", "Entendi") } else { tr!("Close", "Fechar") }, Kind::Primary, &t)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Dismiss))),
                    ),
            )
    }
}
