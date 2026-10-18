//! The composition's editor: the live canvas as the people watching see it, with a box drawn over
//! the source picked, in the window only and never on what is sent. Drag a source to move it, a
//! corner to resize it, Delete takes it off; the right button has everything else. Closing it
//! leaves the composition running as it was left.

use super::picker::Picker;
use super::sidebar::{Menu, MenuEntry};
use crate::media::composite::{self, Composition, Crop, Handle, Layout, Rect};
use crate::media::video::Tile;
use crate::media::voice::Voice;
use crate::prefs::set_prefs;
use crate::theme::{current, px, radius};
use crate::ui::overlay::{self, Dismiss, dialog_card};
use crate::widgets::*;
use gpui::{
    App, AppContext, Bounds, Context, CursorStyle, DispatchPhase, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, Render,
    SharedString, StatefulInteractiveElement, StyleRefinement, Styled, WeakEntity, Window, canvas, div,
};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// What dragging the picked source's grips does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Its corners resize it, its shape kept.
    Arrange,
    /// Every edge trims it in place.
    Crop,
    /// Every grip stretches it.
    Stretch,
}

/// A drag under way: the source, the grip (none to move it), and where it all started.
struct Drag {
    id: u64,
    handle: Option<Handle>,
    from: (f32, f32),
    rect: Rect,
    crop: Crop,
}

pub struct Composer {
    focus: FocusHandle,
    comp: Arc<Composition>,
    tile: Entity<Tile>,
    selected: Option<u64>,
    mode: Mode,
    drag: Option<Drag>,
    /// Where the canvas is in the window, as last laid out.
    stage: Rc<Cell<Bounds<Pixels>>>,
    /// The layout's version last drawn: a new frame draws the editor again only when it changed
    /// (a source just placed, or gone); otherwise the picture alone is drawn again.
    drawn: u64,
}

/// What a menu entry does to the composer when picked.
type MenuAction = Box<dyn Fn(&mut Composer, &mut Window, &mut Context<Composer>)>;

impl EventEmitter<Dismiss> for Composer {}

impl Focusable for Composer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Composer {
    /// Opens the editor over what you share, when that is a composition.
    pub fn open(voice: Entity<Voice>, window: &mut Window, cx: &mut App) {
        let v = voice.read(cx);
        let (Some(comp), Some(tile)) = (v.composition(), v.shares.screen.as_ref().map(|s| s.tile.clone())) else { return };
        let view = cx.new(|cx: &mut Context<Composer>| {
            comp.editing.store(true, Ordering::Relaxed);
            cx.observe(&tile, |this, _, cx| {
                if this.comp.version() != this.drawn {
                    cx.notify();
                }
            })
            .detach();
            // Nothing left to edit once the composition stops going out.
            cx.observe(&voice, |this, voice, cx| {
                if voice.read(cx).composition().is_none_or(|c| !Arc::ptr_eq(&c, &this.comp)) {
                    cx.emit(Dismiss);
                }
            })
            .detach();
            cx.observe_release(&voice, |_, _, cx| cx.emit(Dismiss)).detach();
            cx.on_release(|this, cx| {
                this.comp.editing.store(false, Ordering::Relaxed);
                this.save(cx);
            })
            .detach();
            Composer {
                focus: cx.focus_handle(),
                comp,
                tile,
                selected: None,
                mode: Mode::Arrange,
                drag: None,
                stage: Default::default(),
                drawn: 0,
            }
        });
        overlay::open_dialog(view, window, cx);
    }

    /// Kept for the next composition the picker starts.
    fn save(&self, cx: &mut App) {
        let layout = composite::saved(&self.comp.layout());
        set_prefs(cx, |p| p.composite_layout = layout);
    }

    fn change(&mut self, f: impl FnOnce(&mut Layout), cx: &mut Context<Self>) {
        self.comp.edit(f);
        self.save(cx);
        cx.notify();
    }

    /// A point of the window as a point of the canvas, in fractions of it.
    fn spot(&self, p: Point<Pixels>) -> (f32, f32) {
        let b = self.stage.get();
        let (w, h) = (f32::from(b.size.width).max(1.), f32::from(b.size.height).max(1.));
        (f32::from(p.x - b.origin.x) / w, f32::from(p.y - b.origin.y) / h)
    }

    /// Picks what is under the pointer (or keeps the picked one, for a grip) and starts dragging it.
    fn press(&mut self, at: Point<Pixels>, handle: Option<Handle>, cx: &mut Context<Self>) {
        let from = self.spot(at);
        let layout = self.comp.layout();
        let id = match handle {
            Some(_) => self.selected,
            None => {
                let hit = layout.hit(from.0, from.1);
                if hit != self.selected {
                    self.mode = Mode::Arrange;
                }
                self.selected = hit;
                hit
            }
        };
        self.drag = id.and_then(|id| layout.layer(id)).map(|l| Drag { id: l.id, handle, from, rect: l.rect, crop: l.crop });
        drop(layout);
        cx.notify();
    }

    fn drag_to(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(d) = &self.drag else { return };
        let (x, y) = self.spot(at);
        let (dx, dy) = (x - d.from.0, y - d.from.1);
        let placed = match (d.handle, self.mode) {
            (None, _) => Some((composite::moved(d.rect, dx, dy), d.crop)),
            (Some(h), Mode::Crop) => Some(composite::cropped(d.rect, d.crop, h, dx, dy)),
            (Some(h), Mode::Stretch) => composite::stretched(d.rect, h, dx, dy).map(|r| (r, d.crop)),
            (Some(h), Mode::Arrange) => composite::resized(d.rect, h, dx, dy).map(|r| (r, d.crop)),
        };
        let Some((rect, crop)) = placed else { return };
        let id = d.id;
        self.comp.edit(|l| l.set(id, rect, crop));
        cx.notify();
    }

    fn release(&mut self, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            self.save(cx);
            cx.notify();
        }
    }

    /// Another source, on top, through the picker.
    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (comp, this) = (self.comp.clone(), cx.entity().downgrade());
        let picker = cx.new(|cx| {
            Picker::adding(cx, move |source, name, app, _, cx| {
                let id = comp.edit(|l| l.add(source, name, app));
                let _ = this.update(cx, |c, cx| {
                    (c.selected, c.mode) = (Some(id), Mode::Arrange);
                    cx.notify();
                });
            })
        });
        overlay::open_dialog(picker, window, cx);
    }

    /// The right button: what can be done to the source under the pointer, and adding another.
    fn menu(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let (x, y) = self.spot(at);
        let picked = {
            let layout = self.comp.layout();
            layout.hit(x, y).and_then(|id| layout.layer(id).cloned())
        };
        if picked.as_ref().map(|l| l.id) != self.selected {
            self.mode = Mode::Arrange;
        }
        self.selected = picked.as_ref().map(|l| l.id);
        cx.notify();
        let this = cx.entity().downgrade();
        let entry = |glyph: &'static str, text: SharedString, danger: bool, f: MenuAction| {
            let this: WeakEntity<Composer> = this.clone();
            MenuEntry::item(glyph, text, danger, move |window, cx| {
                let _ = this.update(cx, |c, cx| f(c, window, cx));
            })
        };
        let mut items = Vec::new();
        if let Some(l) = picked {
            let id = l.id;
            let mode = |m: Mode| -> MenuAction {
                Box::new(move |c, _, cx| {
                    c.mode = if c.mode == m { Mode::Arrange } else { m };
                    cx.notify();
                })
            };
            let op = |f: fn(&mut Layout, u64)| -> MenuAction { Box::new(move |c, _, cx| c.change(|layout| f(layout, id), cx)) };
            let size = |w: f32| -> MenuAction { Box::new(move |c, _, cx| c.change(|layout| layout.scale_to(id, w), cx)) };
            let cropping = self.mode == Mode::Crop;
            let stretching = self.mode == Mode::Stretch;
            items.push(entry(
                "scan",
                if cropping { tr!("Finish cropping", "Terminar o corte") } else { tr!("Crop", "Cortar") }.into(),
                false,
                mode(Mode::Crop),
            ));
            if l.crop.any() {
                items.push(entry("restart", tr!("Undo the crop", "Desfazer o corte").into(), false, op(Layout::reset_crop)));
            }
            items.push(MenuEntry::rule());
            items.push(entry(
                "expand",
                if stretching {
                    tr!("Finish free resize", "Terminar o redimensionamento livre")
                } else {
                    tr!("Free resize (stretch)", "Redimensionar livre (esticar)")
                }
                .into(),
                false,
                mode(Mode::Stretch),
            ));
            items.push(entry("minimize", tr!("Fit the canvas", "Encaixar no quadro").into(), false, op(Layout::fit)));
            items.push(entry("maximize", tr!("Fill the canvas", "Preencher o quadro").into(), false, op(Layout::fill)));
            items.push(entry("monitor", tr!("Original size (1:1)", "Tamanho original (1:1)").into(), false, op(Layout::original_size)));
            for (pct, w) in [(75, 0.75), (50, 0.5), (25, 0.25)] {
                items.push(entry("shrink", trf!("{}% of the width", "{}% da largura", pct).into(), false, size(w)));
            }
            items.push(MenuEntry::rule());
            items.push(entry("chevron-up", tr!("Bring to front", "Trazer para a frente").into(), false, op(Layout::to_front)));
            items.push(entry("chevron-up", tr!("Bring forward", "Avançar uma camada").into(), false, op(|l, id| l.step(id, true))));
            items.push(entry("chevron-down", tr!("Send backward", "Recuar uma camada").into(), false, op(|l, id| l.step(id, false))));
            items.push(entry("chevron-down", tr!("Send to back", "Enviar para trás").into(), false, op(Layout::to_back)));
            items.push(entry(
                "delete",
                trf!("Remove {}", "Remover {}", short(&l.name)).into(),
                true,
                op(|l, id| {
                    l.remove(id);
                }),
            ));
            items.push(MenuEntry::rule());
        }
        items.push(entry(
            "plus",
            tr!("Add a screen, window or camera…", "Adicionar uma tela, janela ou câmera…").into(),
            false,
            Box::new(|c, window, cx| c.add(window, cx)),
        ));
        overlay::open_menu(cx.new(|_| Menu { items }), at, cx);
    }

    fn on_key(&mut self, e: &KeyDownEvent, cx: &mut Context<Self>) {
        match e.keystroke.key.as_str() {
            "delete" | "backspace" => {
                if let Some(id) = self.selected.take() {
                    self.mode = Mode::Arrange;
                    self.change(
                        |l| {
                            l.remove(id);
                        },
                        cx,
                    );
                    cx.stop_propagation();
                }
            }
            // Out of cropping or stretching first; the editor closes on the next.
            "escape" if self.mode != Mode::Arrange => {
                self.mode = Mode::Arrange;
                cx.stop_propagation();
                cx.notify();
            }
            _ => {}
        }
    }
}

/// A window's title short enough for a menu, which a path or a browser tab easily isn't.
fn short(name: &str) -> String {
    const MOST: usize = 28;
    match name.char_indices().nth(MOST) {
        Some((at, _)) => format!("{}…", name[..at].trim_end()),
        None => name.to_string(),
    }
}

fn cursor(h: Handle) -> CursorStyle {
    match h {
        Handle::N | Handle::S => CursorStyle::ResizeUpDown,
        Handle::E | Handle::W => CursorStyle::ResizeLeftRight,
        Handle::Nw | Handle::Se => CursorStyle::ResizeUpLeftDownRight,
        Handle::Ne | Handle::Sw => CursorStyle::ResizeUpRightDownLeft,
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = current();
        self.drawn = self.comp.version();
        let (canvas_size, layers) = {
            let l = self.comp.layout();
            (l.canvas, l.layers.clone())
        };
        // One just added stays picked until it has a place to show it at; one gone is let go.
        if self.selected.is_some_and(|id| !layers.iter().any(|l| l.id == id)) {
            (self.selected, self.mode) = (None, Mode::Arrange);
        }
        let picked = self.selected.and_then(|id| layers.iter().find(|l| l.id == id && l.placed)).cloned();
        // As big as the window lets it be, in the canvas's shape.
        let aspect = canvas_size.0.max(1) as f32 / canvas_size.1.max(1) as f32;
        let view = window.viewport_size();
        let s = crate::theme::scale();
        let (vw, vh) = (f32::from(view.width) / s, f32::from(view.height) / s);
        let w = (vw - 120.).min((vh - 250.) * aspect).clamp(320., 1280.);
        let h = w / aspect;

        let (bounds, this) = (self.stage.clone(), cx.entity().downgrade());
        let mut stage = div()
            .id("composer-stage")
            .relative()
            .flex_none()
            .w(px(w))
            .h(px(h))
            .rounded(px(radius::CARD))
            .overflow_hidden()
            .bg(gpui::black())
            .border_1()
            .border_color(t.stroke)
            .child(self.tile.clone().cached(StyleRefinement::default().size_full()))
            .child(
                canvas(
                    move |b, _, _| bounds.set(b),
                    // Moves and the release anywhere in the window: a drag may leave the canvas.
                    move |_, _, window, _| {
                        let (on_move, on_up) = (this.clone(), this);
                        window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                            if phase == DispatchPhase::Bubble && e.pressed_button == Some(MouseButton::Left) {
                                let _ = on_move.update(cx, |c, cx| c.drag_to(e.position, cx));
                            }
                        });
                        window.on_mouse_event(move |e: &MouseUpEvent, phase, _, cx| {
                            if phase == DispatchPhase::Bubble && e.button == MouseButton::Left {
                                let _ = on_up.update(cx, |c, cx| c.release(cx));
                            }
                        });
                    },
                )
                .absolute()
                .inset_0(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, window, cx| {
                    this.focus.focus(window, cx);
                    this.press(e.position, None, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, e: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.menu(e.position, cx);
                }),
            );
        if layers.is_empty() {
            stage = stage.child(div().absolute().inset_0().flex().items_center().justify_center().child(caption(
                tr!(
                    "Right-click to add a screen, window or camera.",
                    "Clique com o botão direito para adicionar uma tela, janela ou câmera."
                ),
                gpui::white().opacity(0.7),
            )));
        }
        if let Some(l) = &picked {
            let r = l.rect;
            let (bx, by, bw, bh) = (r.x * w, r.y * h, r.w * w, r.h * h);
            let color = if self.mode == Mode::Crop { t.caution } else { t.accent };
            let tag = match self.mode {
                Mode::Crop => trf!("Cropping {}", "Cortando {}", l.name),
                Mode::Stretch => trf!("Free resize: {}", "Redimensionamento livre: {}", l.name),
                Mode::Arrange => l.name.clone(),
            };
            let mut select = div().absolute().left(px(bx)).top(px(by)).w(px(bw)).h(px(bh)).border_2().border_color(color).child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .max_w_full()
                    .truncate()
                    .px(px(6.))
                    .py(px(2.))
                    .bg(color)
                    .text_size(px(11.5))
                    .text_color(t.on_accent)
                    .child(tag),
            );
            for (i, handle) in Handle::ALL.into_iter().enumerate().filter(|(_, h)| self.mode != Mode::Arrange || h.corner()) {
                let (fx, fy) = handle.spot();
                select = select.child(
                    div()
                        .id(("composer-grip", i))
                        .absolute()
                        .left(px(fx * bw - 7.))
                        .top(px(fy * bh - 7.))
                        .size(px(14.))
                        .rounded(px(3.))
                        .bg(gpui::white())
                        .border_2()
                        .border_color(color)
                        .cursor(cursor(handle))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                this.focus.focus(window, cx);
                                this.press(e.position, Some(handle), cx);
                            }),
                        ),
                );
            }
            stage = stage.child(select);
        }

        dialog_card(&t, w + 44.)
            .key_context("Composer")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| this.on_key(e, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(22.))
                    .pt(px(18.))
                    .pb(px(14.))
                    .child(
                        div().flex_1().flex().flex_col().gap(px(2.)).child(title(tr!("Composition", "Composição"), t.text)).child(caption(
                            tr!(
                                "Live as you edit. Drag to move, drag a corner to resize, right-click for more.",
                                "Ao vivo enquanto você edita. Arraste para mover, puxe um canto para redimensionar, botão direito para mais."
                            ),
                            t.text2,
                        )),
                    )
                    .child(
                        icon_label_button("composer-add", "plus", tr!("Add a source", "Adicionar uma fonte"), Kind::Standard, &t)
                            .on_click(cx.listener(|this, _, window, cx| this.add(window, cx))),
                    )
                    .child(
                        button("composer-done", tr!("Done", "Pronto"), Kind::Primary, &t).on_click(cx.listener(|_, _, _, cx| cx.emit(Dismiss))),
                    ),
            )
            .child(div().px(px(22.)).child(stage))
            .child(div().px(px(22.)).pt(px(10.)).pb(px(16.)).child(caption(
                tr!(
                    "Delete removes the source picked. Closing this keeps the composition going out as it is.",
                    "Delete remove a fonte escolhida. Fechar isto mantém a composição no ar como está."
                ),
                t.text3,
            )))
    }
}
