//! What floats over the window: dialogs on a scrim, menus at the pointer, toasts. Any view
//! reaches them through `ui::overlay::open_dialog` and `open_menu`, which find the root window
//! view by a global handle.

use super::Root;
use crate::text_field::{TextField, TextFieldEvent};
use crate::theme::{Theme, current, px, radius};
use crate::widgets::*;
use gpui::prelude::FluentBuilder;
use gpui::{
    Animation, AnimationExt, AnyElement, AnyView, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, Global,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, Point, Render, SharedString, StatefulInteractiveElement, Styled,
    WeakEntity, Window, anchored, deferred, div,
};
use std::rc::Rc;
use std::time::Duration;

pub struct Toast {
    pub id: usize,
    pub text: SharedString,
}

/// Emitted by a dialog or menu to close itself.
pub struct Dismiss;

enum Layer {
    /// `dismiss` tells the dialog it was closed from outside (Escape, a click on the scrim), as
    /// if it had closed itself, so what it stands for ends too: a call stops ringing.
    /// `focus` is held by the dialog's frame: Tab goes round the controls inside it, never to the
    /// window behind the scrim.
    Dialog { view: AnyView, dismiss: Rc<dyn Fn(&mut App)>, focus: FocusHandle },
    Menu {
        at: Point<Pixels>,
        anchor: gpui::Anchor,
        view: AnyView,
        /// Held by the menu's frame, not a stop itself: the arrows walk the items inside it.
        focus: FocusHandle,
        /// Where focus was when it opened, to go back to when it closes; taken once it has.
        restore: Option<FocusHandle>,
        /// Focus has not been moved in yet.
        fresh: bool,
    },
}

impl Layer {
    fn view(&self) -> &AnyView {
        match self {
            Layer::Dialog { view, .. } | Layer::Menu { view, .. } => view,
        }
    }

    /// What is left of it to draw while it fades out.
    fn ghost(&self) -> Ghost {
        match self {
            Layer::Dialog { view, .. } => Ghost::Dialog(view.clone()),
            Layer::Menu { at, anchor, view, .. } => Ghost::Menu { at: *at, anchor: *anchor, view: view.clone() },
        }
    }
}

/// A dialog or a menu that closed, drawn a moment longer as it fades, taking no clicks.
enum Ghost {
    Dialog(AnyView),
    Menu { at: Point<Pixels>, anchor: gpui::Anchor, view: AnyView },
}

#[derive(Default)]
pub struct Overlay {
    layers: Vec<Layer>,
    /// A menu that closed, with where focus goes back to if it was still inside it.
    closed_menu: Option<(FocusHandle, FocusHandle)>,
    /// Layers that closed and are fading out, with when.
    leaving: Vec<(Ghost, std::time::Instant)>,
    /// Draws again once they have faded, to let them go.
    leaving_wake: Option<gpui::Task<()>>,
}

impl Overlay {
    /// Closes the top layer; false when there was none.
    pub fn close_top(&mut self, cx: &mut App) -> bool {
        match self.layers.pop() {
            None => false,
            Some(dialog @ Layer::Dialog { .. }) => {
                self.bury(&dialog);
                let Layer::Dialog { dismiss, .. } = dialog else { unreachable!() };
                // Later: the dialog's own handlers may update the root, which is being updated now.
                cx.defer(move |cx| dismiss(cx));
                true
            }
            Some(menu @ Layer::Menu { .. }) => {
                self.bury(&menu);
                self.forget_menu(menu);
                true
            }
        }
    }

    fn bury(&mut self, layer: &Layer) {
        self.leaving.push((layer.ghost(), std::time::Instant::now()));
    }

    pub fn has_menu(&self) -> bool {
        self.layers.iter().any(|l| matches!(l, Layer::Menu { .. }))
    }

    /// The frame of the top dialog or menu, which Tab stays inside while it is open.
    pub fn trap(&self) -> Option<FocusHandle> {
        self.layers.last().map(|l| match l {
            Layer::Dialog { focus, .. } | Layer::Menu { focus, .. } => focus.clone(),
        })
    }

    /// Takes the menus away, keeping where focus goes back to.
    fn close_menus(&mut self) {
        let (menus, rest): (Vec<Layer>, Vec<Layer>) =
            std::mem::take(&mut self.layers).into_iter().partition(|l| matches!(l, Layer::Menu { .. }));
        self.layers = rest;
        menus.into_iter().for_each(|m| {
            self.bury(&m);
            self.forget_menu(m)
        });
    }

    fn remove(&mut self, id: gpui::EntityId) {
        let (gone, rest): (Vec<Layer>, Vec<Layer>) = std::mem::take(&mut self.layers).into_iter().partition(|l| l.view().entity_id() == id);
        self.layers = rest;
        gone.into_iter().for_each(|m| {
            self.bury(&m);
            self.forget_menu(m)
        });
    }

    fn forget_menu(&mut self, layer: Layer) {
        if let Layer::Menu { focus, restore: Some(back), .. } = layer {
            self.closed_menu = Some((focus, back));
        }
    }

    /// After a menu closed: focus goes back where it was, unless what the menu did moved it
    /// elsewhere (a dialog it opened, a field it focused).
    pub fn restore_focus(&mut self, window: &mut Window, cx: &mut App) {
        let Some((menu, back)) = self.closed_menu.take() else { return };
        if window.focused(cx).is_none() || menu.contains_focused(window, cx) {
            back.focus(window, cx);
        }
    }

    /// The open menu takes focus the first time it is drawn, unless something inside it (a
    /// search field) already has.
    pub fn focus_new_menu(&mut self, window: &mut Window, cx: &mut App) {
        for layer in &mut self.layers {
            if let Layer::Menu { focus, restore, fresh, .. } = layer
                && *fresh
            {
                *fresh = false;
                if restore.is_none() {
                    *restore = window.focused(cx);
                }
                let focus = focus.clone();
                window.defer(cx, move |window, cx| {
                    if !focus.contains_focused(window, cx) {
                        focus.focus(window, cx);
                    }
                });
            }
        }
    }
}

/// Where focus goes inside a dialog or a menu.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Next,
    Prev,
    First,
    Last,
}

/// Up and Down walk a menu's items, wrapping around; Home and End go to the first and the last.
fn walk_menu(menu: &FocusHandle, key: &str, window: &mut Window, cx: &mut App) -> bool {
    let step = match key {
        "down" => Step::Next,
        "up" => Step::Prev,
        "home" => Step::First,
        "end" => Step::Last,
        _ => return false,
    };
    walk_within(menu, step, window, cx);
    true
}

/// Moves focus among the stops inside `frame`, wrapping around at either end. They are found by
/// walking forward from the frame through the window's stops, where a layer's own come together.
pub fn walk_within(frame: &FocusHandle, step: Step, window: &mut Window, cx: &mut App) {
    let was = window.focused(cx);
    frame.focus(window, cx);
    let mut items: Vec<FocusHandle> = Vec::new();
    for _ in 0..256 {
        window.focus_next(cx);
        match window.focused(cx) {
            Some(h) if frame.contains(&h, window) && h != *frame && !items.contains(&h) => items.push(h),
            _ => break,
        }
    }
    let n = items.len();
    if n == 0 {
        match was {
            Some(h) => h.focus(window, cx),
            None => frame.focus(window, cx),
        }
        return;
    }
    let at = was.and_then(|w| items.iter().position(|h| *h == w));
    let to = match (step, at) {
        (Step::Next, Some(i)) => (i + 1) % n,
        (Step::Prev, Some(i)) => (i + n - 1) % n,
        (Step::Prev | Step::Last, _) => n - 1,
        _ => 0,
    };
    items[to].focus(window, cx);
}

pub struct RootHandle(pub WeakEntity<Root>);

impl Global for RootHandle {}

fn with_root(cx: &mut App, f: impl FnOnce(&mut Root, &mut Context<Root>)) {
    let Some(root) = cx.try_global::<RootHandle>().and_then(|h| h.0.upgrade()) else { return };
    root.update(cx, f);
}

pub fn toast(text: impl Into<SharedString>, cx: &mut App) {
    let text = text.into();
    with_root(cx, |root, cx| root.toast(text, cx));
}

/// Shows a dialog over everything. It closes on Escape, a click on the scrim, or `Dismiss`.
pub fn open_dialog<V: Render + EventEmitter<Dismiss> + Focusable>(view: Entity<V>, window: &mut Window, cx: &mut App) {
    view.focus_handle(cx).focus(window, cx);
    let any: AnyView = view.clone().into();
    let weak = view.downgrade();
    let dismiss: Rc<dyn Fn(&mut App)> = Rc::new(move |cx| {
        if let Some(v) = weak.upgrade() {
            v.update(cx, |_, cx| cx.emit(Dismiss));
        }
    });
    with_root(cx, move |root, cx| {
        close_on_dismiss(&view, cx);
        root.overlay.layers.push(Layer::Dialog { view: any, dismiss, focus: cx.focus_handle() });
        cx.notify();
    });
}

/// Takes `view`'s layer away once it says `Dismiss`.
fn close_on_dismiss<V: EventEmitter<Dismiss>>(view: &Entity<V>, cx: &mut Context<Root>) {
    cx.subscribe(view, |root: &mut Root, v, _: &Dismiss, cx| {
        root.overlay.remove(v.entity_id());
        cx.notify();
    })
    .detach();
}

/// Shows a menu with its corner at `at`. It closes on a click anywhere else.
pub fn open_menu<V: Render + EventEmitter<Dismiss>>(view: Entity<V>, at: Point<Pixels>, cx: &mut App) {
    show_menu(view, at, gpui::Anchor::TopLeft, cx);
}

/// A menu that grows upward from `at`, for buttons along the bottom of the window, where the
/// button's own tooltip would otherwise sit on the first item.
pub fn open_menu_above<V: Render + EventEmitter<Dismiss>>(view: Entity<V>, at: Point<Pixels>, cx: &mut App) {
    show_menu(view, at - gpui::point(px(0.), px(10.)), gpui::Anchor::BottomLeft, cx);
}

fn show_menu<V: Render + EventEmitter<Dismiss>>(view: Entity<V>, at: Point<Pixels>, anchor: gpui::Anchor, cx: &mut App) {
    let any: AnyView = view.clone().into();
    with_root(cx, move |root, cx| {
        close_on_dismiss(&view, cx);
        root.overlay.close_menus();
        // Its predecessor's focus to go back to, not the predecessor.
        let restore = root.overlay.closed_menu.take().map(|(_, back)| back);
        root.overlay.layers.push(Layer::Menu { at, anchor, view: any, focus: cx.focus_handle(), restore, fresh: true });
        cx.notify();
    });
}

impl Root {
    pub fn render_overlay(&mut self, t: &Theme, cx: &mut Context<Root>) -> Vec<AnyElement> {
        let mut out = self.render_leaving(t, cx);
        let top = self.overlay.layers.len();
        for (i, layer) in self.overlay.layers.iter().enumerate() {
            match layer {
                Layer::Dialog { view, focus, .. } => out.push(
                    div()
                        .id(("scrim", view.entity_id()))
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .p(px(24.))
                        .bg(t.scrim)
                        .occlude()
                        .when(i + 1 == top, |d| {
                            d.on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|root, _, _, cx| {
                                    root.overlay.close_top(cx);
                                    cx.notify();
                                }),
                            )
                        })
                        .child(
                            div()
                                .id(("dialog", view.entity_id()))
                                .track_focus(focus)
                                .max_h_full()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .child(view.clone())
                                // It settles in rather than appears: a short rise and fade.
                                .with_animation(
                                    ("dialog-in", view.entity_id()),
                                    Animation::new(Duration::from_millis(140)).with_easing(gpui::ease_out_quint()),
                                    |d, p| d.opacity(p).mt(px(10. * (1. - p))),
                                ),
                        )
                        .with_animation(
                            ("scrim-in", view.entity_id()),
                            Animation::new(Duration::from_millis(120)).with_easing(gpui::ease_out_quint()),
                            |d, p| d.opacity(p),
                        )
                        .into_any_element(),
                ),
                Layer::Menu { at, anchor, view, focus, .. } => out.push(
                    deferred(
                        div()
                            .id(("menu-catcher", i))
                            .absolute()
                            .inset_0()
                            .occlude()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|root, _, _, cx| {
                                    root.overlay.close_menus();
                                    cx.notify();
                                }),
                            )
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(|root, _, _, cx| {
                                    root.overlay.close_menus();
                                    cx.notify();
                                }),
                            )
                            .child(
                                anchored().anchor(*anchor).position(*at).snap_to_window_with_margin(px(8.)).child(
                                    div()
                                        .track_focus(focus)
                                        .on_key_down({
                                            let focus = focus.clone();
                                            move |e: &gpui::KeyDownEvent, window, cx| {
                                                let k = &e.keystroke;
                                                if !k.modifiers.modified() && walk_menu(&focus, &k.key, window, cx) {
                                                    cx.stop_propagation();
                                                }
                                            }
                                        })
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                        .child(view.clone())
                                        // It drops into place from the side it opens on, as it fades in.
                                        .with_animation(
                                            ("menu-in", view.entity_id()),
                                            Animation::new(Duration::from_millis(130)).with_easing(gpui::ease_out_quint()),
                                            {
                                                let up = matches!(anchor, gpui::Anchor::BottomLeft);
                                                move |d, p| {
                                                    let shift = 6. * (1. - p);
                                                    d.opacity(p).relative().top(px(if up { shift } else { -shift }))
                                                }
                                            },
                                        ),
                                ),
                            ),
                    )
                    .with_priority(1)
                    .into_any_element(),
                ),
            }
        }
        out
    }

    /// The layers that closed, fading out where they were: a dialog sinks a little, a menu draws
    /// back toward where it opened from. What is under them takes no clicks until they are gone.
    fn render_leaving(&mut self, t: &Theme, cx: &mut Context<Root>) -> Vec<AnyElement> {
        let leave = crate::motion::LEAVE;
        let overlay = &mut self.overlay;
        overlay.leaving.retain(|(_, at)| at.elapsed() < leave);
        if !overlay.leaving.is_empty() && overlay.leaving_wake.is_none() {
            overlay.leaving_wake = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(leave + Duration::from_millis(20)).await;
                let _ = this.update(cx, |root, cx| {
                    root.overlay.leaving_wake = None;
                    cx.notify();
                });
            }));
        }
        let fade = || Animation::new(leave).with_easing(gpui::ease_out_quint());
        let blocker = || div().absolute().inset_0().occlude();
        overlay
            .leaving
            .iter()
            .map(|(ghost, _)| match ghost {
                Ghost::Dialog(view) => {
                    let id = view.entity_id();
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .p(px(24.))
                        .bg(t.scrim)
                        .occlude()
                        .child(
                            div()
                                .max_h_full()
                                .child(view.clone())
                                .with_animation(("dialog-out", id), fade(), |d, p| d.opacity(1. - p).mt(px(8. * p))),
                        )
                        .child(blocker())
                        .with_animation(("scrim-out", id), fade(), |d, p| d.opacity(1. - p))
                        .into_any_element()
                }
                Ghost::Menu { at, anchor, view } => {
                    let up = matches!(anchor, gpui::Anchor::BottomLeft);
                    deferred(div().absolute().inset_0().child(
                        anchored().anchor(*anchor).position(*at).snap_to_window_with_margin(px(8.)).child(
                            div().relative().child(view.clone()).child(blocker()).with_animation(
                                ("menu-out", view.entity_id()),
                                fade(),
                                move |d, p| {
                                    let shift = 4. * p;
                                    d.opacity(1. - p).top(px(if up { shift } else { -shift }))
                                },
                            ),
                        ),
                    ))
                    .with_priority(1)
                    .into_any_element()
                }
            })
            .collect()
    }
}

/// The card a dialog sits on.
pub fn dialog_card(t: &Theme, width: f32) -> gpui::Div {
    div()
        .w(px(width))
        .max_w_full()
        .flex()
        .flex_col()
        .rounded(px(radius::PANE + 2.))
        .bg(t.pane)
        .border_1()
        .border_color(t.stroke_strong)
        .shadow_lg()
        .overflow_hidden()
}

/// A menu's surface.
pub fn menu_card(t: &Theme) -> gpui::Div {
    div()
        .min_w(px(200.))
        .max_w(px(320.))
        .flex()
        .flex_col()
        .p(px(5.))
        .rounded(px(radius::CONTROL + 2.))
        .bg(t.popover)
        .border_1()
        .border_color(t.stroke_strong)
        .shadow_lg()
}

pub fn menu_item(
    id: impl Into<gpui::ElementId>,
    glyph: Option<&'static str>,
    text: impl Into<SharedString>,
    danger: bool,
    t: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let color = if danger { t.critical } else { t.text };
    let hover = if danger { t.tint(t.critical) } else { t.layer_hover };
    // Under the keyboard the row fills as it does under the pointer, and a little more.
    let focus = if danger { t.critical.opacity(0.22) } else { t.layer_hover };
    div()
        .id(id.into())
        .flex()
        .items_center()
        .gap(px(10.))
        .h(px(32.))
        .px(px(10.))
        .rounded(px(radius::INNER))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .tab_index(0)
        .focus_visible(move |s| s.bg(focus))
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .when_some(glyph, |d, g| d.child(icon(g, 15., if danger { t.critical } else { t.text2 })))
        .child(body(text, color))
}

pub fn menu_rule(t: &Theme) -> gpui::Div {
    div().my(px(4.)).mx(px(6.)).h(px(1.)).bg(t.stroke)
}

// The general-purpose prompt: a title, some text, a few fields, OK and Cancel.

pub enum Field {
    Text {
        label: &'static str,
        value: String,
        placeholder: &'static str,
        secret: bool,
        multiline: bool,
        max: usize,
    },
    Choice {
        label: &'static str,
        options: Vec<(String, String)>,
        picked: usize,
    },
    /// A one-line field that may be left empty.
    Optional {
        label: &'static str,
        value: String,
        placeholder: &'static str,
        max: usize,
    },
    /// A checkbox; its value is "1" when ticked and empty when not.
    Check {
        label: &'static str,
        on: bool,
    },
}

type OnOk = Rc<dyn Fn(Vec<String>, &mut Window, &mut App) -> Option<String>>;

pub struct Ask {
    focus: FocusHandle,
    title: SharedString,
    text: Option<SharedString>,
    ok: SharedString,
    danger: bool,
    fields: Vec<AskField>,
    error: Option<String>,
    on_ok: OnOk,
}

enum AskField {
    Text { label: &'static str, field: Entity<TextField>, required: bool },
    Choice { label: &'static str, options: Vec<(String, String)>, picked: usize },
    Check { label: &'static str, on: bool },
}

impl EventEmitter<Dismiss> for Ask {}

impl Focusable for Ask {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.fields
            .iter()
            .find_map(|f| match f {
                AskField::Text { field, .. } => Some(field.focus_handle(cx)),
                _ => None,
            })
            .unwrap_or_else(|| self.focus.clone())
    }
}

impl Ask {
    /// `on_ok` gets the fields' values in order; returning `Some(error)` keeps the dialog open
    /// with that message.
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        title: impl Into<SharedString>,
        text: Option<String>,
        ok: impl Into<SharedString>,
        danger: bool,
        fields: Vec<Field>,
        window: &mut Window,
        cx: &mut App,
        on_ok: impl Fn(Vec<String>, &mut Window, &mut App) -> Option<String> + 'static,
    ) {
        let (title, ok) = (title.into(), ok.into());
        let view = cx.new(|cx: &mut Context<Ask>| {
            let fields = fields
                .into_iter()
                .map(|f| match f {
                    Field::Text { label, value, placeholder, secret, multiline, max } => {
                        let field = cx.new(|cx| {
                            let mut f = TextField::new(cx, multiline, max).placeholder(placeholder);
                            if multiline {
                                f = f.lines(3, 10);
                            }
                            f.set_masked(secret);
                            f.set_text(&value, cx);
                            f
                        });
                        cx.subscribe_in(&field, window, |this: &mut Ask, _, ev: &TextFieldEvent, window, cx| {
                            // Typing clears the last error; Enter may set a new one, which stays.
                            if let TextFieldEvent::Submit = ev {
                                this.confirm(window, cx);
                            } else if this.error.take().is_some() {
                                cx.notify();
                            }
                        })
                        .detach();
                        AskField::Text { label, field, required: !secret }
                    }
                    Field::Choice { label, options, picked } => AskField::Choice { label, options, picked },
                    Field::Check { label, on } => AskField::Check { label, on },
                    Field::Optional { label, value, placeholder, max } => {
                        let field = cx.new(|cx| {
                            let mut f = TextField::new(cx, false, max).placeholder(placeholder);
                            f.set_text(&value, cx);
                            f
                        });
                        cx.subscribe_in(&field, window, |this: &mut Ask, _, ev: &TextFieldEvent, window, cx| {
                            if let TextFieldEvent::Submit = ev {
                                this.confirm(window, cx);
                            }
                        })
                        .detach();
                        AskField::Text { label, field, required: false }
                    }
                })
                .collect();
            Ask { focus: cx.focus_handle(), title, text: text.map(Into::into), ok, danger, fields, error: None, on_ok: Rc::new(on_ok) }
        });
        open_dialog(view, window, cx);
    }

    /// A yes-or-no question.
    pub fn confirm_action(
        title: impl Into<SharedString>,
        text: impl Into<String>,
        ok: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut App,
        on_ok: impl Fn(&mut Window, &mut App) + 'static,
    ) {
        Ask::open(title, Some(text.into()), ok, true, Vec::new(), window, cx, move |_, w, cx| {
            on_ok(w, cx);
            None
        });
    }

    fn values(&self, cx: &App) -> Vec<String> {
        self.fields
            .iter()
            .map(|f| match f {
                AskField::Text { field, .. } => field.read(cx).text(),
                AskField::Choice { options, picked, .. } => options.get(*picked).map(|o| o.0.clone()).unwrap_or_default(),
                AskField::Check { on, .. } => if *on { "1" } else { "" }.into(),
            })
            .collect()
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let values = self.values(cx);
        let missing =
            self.fields.iter().zip(&values).any(|(f, v)| matches!(f, AskField::Text { required: true, .. }) && v.trim().is_empty());
        let single_field_required = self.fields.len() == 1 && missing;
        if single_field_required {
            self.error = Some(tr!("This can't be empty.", "Isso não pode ficar vazio.").into());
            cx.notify();
            return;
        }
        let on_ok = self.on_ok.clone();
        match on_ok(values, window, cx) {
            Some(err) => {
                self.error = Some(err);
                cx.notify();
            }
            None => cx.emit(Dismiss),
        }
    }
}

impl Render for Ask {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = current();
        let mut content = div().flex().flex_col().gap(px(14.)).p(px(22.)).child(title(self.title.clone(), t.text));
        if let Some(text) = &self.text {
            content = content.child(body(text.clone(), t.text2));
        }
        for (i, f) in self.fields.iter().enumerate() {
            content = content.child(match f {
                AskField::Text { label: l, field, .. } => div().flex().flex_col().gap(px(6.)).child(label(*l, &t)).child(field.clone()),
                AskField::Choice { label: l, options, picked } => {
                    let choices: Vec<(usize, SharedString)> = options.iter().enumerate().map(|(i, o)| (i, o.1.clone().into())).collect();
                    let id: &'static str = ["ask-choice-0", "ask-choice-1", "ask-choice-2", "ask-choice-3"][i.min(3)];
                    div().flex().flex_col().gap(px(6.)).child(label(*l, &t)).child(segmented(
                        id,
                        choices,
                        *picked,
                        &t,
                        cx,
                        move |this: &mut Ask, v, _, cx| {
                            if let Some(AskField::Choice { picked, .. }) = this.fields.get_mut(i) {
                                *picked = v;
                            }
                            cx.notify();
                        },
                    ))
                }
                AskField::Check { label: l, on } => {
                    let id: &'static str = ["ask-check-0", "ask-check-1", "ask-check-2", "ask-check-3"][i.min(3)];
                    let on = *on;
                    div().child(
                        div()
                            .id(id)
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .cursor_pointer()
                            .child(checkbox(on, &t))
                            .child(body(*l, t.text2))
                            .on_click(cx.listener(move |this: &mut Ask, _, _, cx| {
                                if let Some(AskField::Check { on, .. }) = this.fields.get_mut(i) {
                                    *on = !*on;
                                }
                                cx.notify();
                            })),
                    )
                }
            });
        }
        if let Some(e) = &self.error {
            content = content.child(caption(e.clone(), t.critical));
        }
        dialog_card(&t, 440.).key_context("Ask").track_focus(&self.focus).child(content).child(
            div()
                .flex()
                .justify_end()
                .gap(px(8.))
                .px(px(22.))
                .py(px(14.))
                .bg(t.layer)
                .border_t_1()
                .border_color(t.stroke)
                .child(
                    button("ask-cancel", tr!("Cancel", "Cancelar"), Kind::Standard, &t)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(Dismiss))),
                )
                .child(
                    button("ask-ok", self.ok.clone(), if self.danger { Kind::Danger } else { Kind::Primary }, &t)
                        .on_click(cx.listener(|this, _, window, cx| this.confirm(window, cx))),
                ),
        )
    }
}
