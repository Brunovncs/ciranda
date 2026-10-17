//! The notice that the microphone changed without anyone choosing it (one unplugged, another
//! plugged in and made Windows' default), with the way back to the one before or to another.

use crate::media::audio::{self, DeviceInfo, MicSwitch};
use crate::media::voice::apply_mic_prefs;
use crate::prefs::set_prefs;
use crate::theme::{current, px, radius};
use crate::ui::overlay::{self, Dismiss, dialog_card};
use crate::widgets::*;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyWindowHandle, App, AppContext, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement, ParentElement,
    Render, StatefulInteractiveElement, Styled, WeakEntity, Window, div,
};

/// Shows every switch on `window` as it happens, a new one in place of the last.
pub fn listen(window: AnyWindowHandle, cx: &mut App) {
    let switches = audio::mic_switches();
    cx.spawn(async move |cx| {
        let mut shown: Option<WeakEntity<MicSwitched>> = None;
        while let Ok(switch) = switches.recv().await {
            let Ok(()) = window.update(cx, |_, window, cx| {
                if let Some(old) = shown.take().and_then(|w| w.upgrade()) {
                    old.update(cx, |_, cx| cx.emit(Dismiss));
                }
                let view = cx.new(|cx| MicSwitched::new(switch, cx));
                shown = Some(view.downgrade());
                overlay::open_dialog(view, window, cx);
            }) else {
                break;
            };
        }
    })
    .detach();
}

pub struct MicSwitched {
    focus: FocusHandle,
    switch: MicSwitch,
    /// The microphones plugged in, once listed.
    devices: Vec<DeviceInfo>,
    choosing: bool,
}

impl EventEmitter<Dismiss> for MicSwitched {}

impl Focusable for MicSwitched {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl MicSwitched {
    fn new(switch: MicSwitch, cx: &mut Context<Self>) -> MicSwitched {
        cx.spawn(async move |this, cx| {
            let devices = cx.background_executor().spawn(async { audio::input_devices() }).await;
            let _ = this.update(cx, |this, cx| {
                this.devices = devices;
                cx.notify();
            });
        })
        .detach();
        MicSwitched { focus: cx.focus_handle(), switch, devices: Vec::new(), choosing: false }
    }

    /// The one before, while it's still plugged in and isn't the one in use.
    fn back_to(&self) -> Option<&DeviceInfo> {
        let from = self.switch.from.as_ref()?;
        (self.switch.to.as_ref().is_none_or(|to| to.id != from.id) && self.devices.iter().any(|d| d.id == from.id)).then_some(from)
    }

    /// Keeps `id` (empty for Windows' default) as the microphone from now on.
    fn pick(&mut self, id: String, cx: &mut Context<Self>) {
        set_prefs(cx, |p| p.voice_input_id = id);
        apply_mic_prefs(cx);
        cx.emit(Dismiss);
    }
}

impl Render for MicSwitched {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = current();
        let (from, to) = (self.switch.from.as_ref(), self.switch.to.as_ref());
        let (heading, text) = match (from, to) {
            (from, Some(to)) => (
                tr!("Your microphone changed", "Seu microfone mudou"),
                match from.filter(|f| f.id != to.id) {
                    Some(f) => trf!("Now using {} instead of {}.", "Agora usando {} no lugar de {}.", to.name, f.name),
                    None => trf!("Now using {}.", "Agora usando {}.", to.name),
                },
            ),
            (Some(from), None) => (
                tr!("Your microphone was disconnected", "Seu microfone foi desconectado"),
                trf!(
                    "{} is gone and there's no other microphone, so nobody can hear you.",
                    "{} saiu e não há outro microfone, então ninguém está ouvindo você.",
                    from.name
                ),
            ),
            (None, None) => {
                (tr!("No microphone", "Nenhum microfone"), tr!("Nobody can hear you.", "Ninguém está ouvindo você.").to_string())
            }
        };
        let mut content = div()
            .flex()
            .flex_col()
            .gap(px(14.))
            .p(px(22.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(icon(if to.is_some() { "mic" } else { "mic-off" }, 18., t.text2))
                    .child(title(heading, t.text)),
            )
            .child(body(text, t.text2));
        if self.choosing {
            let mut options = vec![(String::new(), tr!("Windows default", "Padrão do Windows").to_string())];
            options.extend(self.devices.iter().map(|d| (d.id.clone(), d.name.clone())));
            let mut list = div().id("mic-choices").flex().flex_col().gap(px(1.)).max_h(px(240.)).overflow_y_scroll();
            for (i, (id, name)) in options.into_iter().enumerate() {
                let using = to.is_some_and(|to| to.id == id);
                let hover = t.layer_hover;
                list = list.child(
                    div()
                        .id(("mic-choice", i))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .h(px(34.))
                        .px(px(10.))
                        .rounded(px(radius::INNER + 1.))
                        .cursor_pointer()
                        .hover(move |s| s.bg(hover))
                        .child(div().flex_1().min_w(px(0.)).truncate().child(body(name, t.text)))
                        .when(using, |d| d.child(icon("check", 14., t.accent)))
                        .on_click(cx.listener(move |this, _, _, cx| this.pick(id.clone(), cx))),
                );
            }
            content = content.child(list);
        }
        let back = self.back_to().cloned();
        dialog_card(&t, 440.).key_context("MicSwitched").track_focus(&self.focus).child(content).child(
            div()
                .flex()
                .items_center()
                .justify_end()
                .gap(px(8.))
                .px(px(22.))
                .py(px(14.))
                .bg(t.layer)
                .border_t_1()
                .border_color(t.stroke)
                .when(!self.choosing, |d| {
                    d.child(button("mic-choose", tr!("Choose another", "Escolher outro"), Kind::Subtle, &t).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.choosing = true;
                            cx.notify();
                        },
                    )))
                })
                .child(div().flex_1())
                .when_some(back, |d, back| {
                    d.child(
                        button("mic-back", tr!("Switch back", "Voltar ao anterior"), Kind::Standard, &t)
                            .on_click(cx.listener(move |this, _, _, cx| this.pick(back.id.clone(), cx))),
                    )
                })
                .child(button("mic-ok", tr!("OK", "OK"), Kind::Primary, &t).on_click(cx.listener(|_, _, _, cx| cx.emit(Dismiss)))),
        )
    }
}
