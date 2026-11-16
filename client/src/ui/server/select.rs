//! Selecting the text of messages with the mouse: press on a message's words and drag, within one
//! message or across several, then Ctrl+C (or "Copy selection" on a right click). Shift and a click
//! moves the end of the selection there.
//!
//! Each run of text is a [`Selectable`], which draws its share of the selection under the words and
//! takes the press that starts one. The chat pane owns the [`Selection`]; it is told when a frame
//! begins, and the first run drawn in it listens to the whole window for the drag and for the press
//! elsewhere that drops the selection.

use gpui::{
    App, Bounds, CursorStyle, DispatchPhase, Element, ElementId, EntityId, FocusHandle, GlobalElementId, Hitbox, Hsla, InspectorElementId,
    InteractiveText, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, SharedString,
    TextLayout, Window, fill, point,
};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

/// Where a run of text sits among all the others, in the order they are read: the message (its id,
/// or its place among search results), then the block, the line and the run within the line.
pub type Key = (i64, u32, u32, u32);

type Spot = (Key, usize);

/// What is selected in one chat pane, shared by the pane and every run of text it draws.
#[derive(Clone)]
pub struct Selection(Rc<RefCell<State>>);

struct State {
    anchor: Option<Spot>,
    head: Option<Spot>,
    dragging: bool,
    /// Where the pointer was last seen while dragging.
    pointer: Option<Point<Pixels>>,
    /// The runs drawn this frame, where they are and what they say.
    shown: Vec<(Key, TextLayout, SharedString)>,
    /// Whether this frame's window-wide listeners are in place.
    listening: bool,
    /// The text of every run seen since the selection began, so what scrolled away is still copied.
    texts: BTreeMap<Key, SharedString>,
    view: EntityId,
    focus: FocusHandle,
}

impl Selection {
    pub fn new(view: EntityId, focus: FocusHandle) -> Selection {
        Selection(Rc::new(RefCell::new(State {
            anchor: None,
            head: None,
            dragging: false,
            pointer: None,
            shown: Vec::new(),
            listening: false,
            texts: BTreeMap::new(),
            view,
            focus,
        })))
    }

    /// Called as the pane draws, before any run of text.
    pub fn begin_frame(&self) {
        let mut s = self.0.borrow_mut();
        s.shown.clear();
        s.listening = false;
    }

    pub fn clear(&self) {
        let mut s = self.0.borrow_mut();
        s.anchor = None;
        s.head = None;
        s.dragging = false;
        s.texts.clear();
    }

    /// The selection's two ends, first one first, if it holds anything.
    fn range(&self) -> Option<(Spot, Spot)> {
        let s = self.0.borrow();
        let (a, h) = (s.anchor?, s.head?);
        let (from, to) = if a <= h { (a, h) } else { (h, a) };
        (from != to).then_some((from, to))
    }

    /// Where the pointer is while a drag selects, for the pane to scroll when it is past an edge.
    pub fn dragging_at(&self) -> Option<Point<Pixels>> {
        let s = self.0.borrow();
        if s.dragging { s.pointer } else { None }
    }

    /// Moves the selection's end to the pointer again, after the text under it scrolled.
    pub fn follow_pointer(&self) {
        if let Some(p) = self.dragging_at()
            && let Some(spot) = self.locate(p)
        {
            self.0.borrow_mut().head = Some(spot);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.range().is_none()
    }

    /// The selected text, with a line break between lines and a blank line between paragraphs.
    pub fn text(&self) -> Option<String> {
        let (from, to) = self.range()?;
        let out = join(&self.0.borrow().texts, from, to);
        (!out.is_empty()).then_some(out)
    }

    /// The part of the run at `key` that is selected, and whether the selection goes on past it.
    fn part(&self, key: Key, len: usize) -> Option<(usize, usize, bool)> {
        let (from, to) = self.range()?;
        if key < from.0 || key > to.0 {
            return None;
        }
        let start = if key == from.0 { from.1.min(len) } else { 0 };
        let end = if key == to.0 { to.1.min(len) } else { len };
        (start < end || key < to.0).then_some((start, end, key < to.0))
    }

    /// Notes a run as drawn, and puts the frame's window-wide listeners in place if none are yet.
    fn drawn(&self, key: Key, layout: &TextLayout, text: &SharedString, window: &mut Window) {
        let listen = {
            let mut s = self.0.borrow_mut();
            s.shown.push((key, layout.clone(), text.clone()));
            if s.anchor.is_some() {
                s.texts.insert(key, text.clone());
            }
            !std::mem::replace(&mut s.listening, true)
        };
        if !listen {
            return;
        }
        if self.0.borrow().dragging {
            window.set_window_cursor_style(CursorStyle::IBeam);
        }
        // A press anywhere but on a message's words lets the selection go; on them, the run pressed
        // starts a new one, after this.
        let sel = self.clone();
        window.on_mouse_event(move |e: &MouseDownEvent, phase, _, cx| {
            if phase != DispatchPhase::Capture || e.button != MouseButton::Left {
                return;
            }
            let on_text = sel.0.borrow().shown.iter().any(|(_, l, _)| l.bounds().contains(&e.position));
            if !(on_text && e.modifiers.shift) && sel.0.borrow().anchor.is_some() {
                sel.clear();
                cx.notify(sel.0.borrow().view);
            }
        });
        let sel = self.clone();
        window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
            if phase != DispatchPhase::Bubble || !sel.0.borrow().dragging {
                return;
            }
            if e.pressed_button != Some(MouseButton::Left) {
                sel.0.borrow_mut().dragging = false;
            } else {
                sel.0.borrow_mut().pointer = Some(e.position);
                sel.follow_pointer();
            }
            cx.notify(sel.0.borrow().view);
        });
        let sel = self.clone();
        window.on_mouse_event(move |e: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble && e.button == MouseButton::Left && sel.0.borrow().dragging {
                sel.0.borrow_mut().dragging = false;
                cx.notify(sel.0.borrow().view);
            }
        });
    }

    /// The place in the text nearest the pointer: in the run under it, or else at the start of the
    /// first run below it, or the end of the last one.
    fn locate(&self, p: Point<Pixels>) -> Option<Spot> {
        let s = self.0.borrow();
        let mut shown: Vec<_> = s.shown.iter().collect();
        shown.sort_by_key(|(k, ..)| *k);
        if let Some((k, l, _)) = shown.iter().find(|(_, l, _)| l.bounds().contains(&p)) {
            return Some((*k, index_at(l, p)));
        }
        if let Some((k, l, _)) = shown.iter().find(|(_, l, _)| l.bounds().top() <= p.y && p.y < l.bounds().bottom()) {
            return Some((*k, index_at(l, p)));
        }
        if let Some((k, ..)) = shown.iter().find(|(_, l, _)| l.bounds().top() > p.y) {
            return Some((*k, 0));
        }
        shown.last().map(|(k, _, t)| (*k, t.len()))
    }

    /// The press that starts a selection, or with Shift moves its end.
    fn press(&self, key: Key, ix: usize, at: Point<Pixels>, extend: bool, window: &mut Window, cx: &mut App) {
        let mut s = self.0.borrow_mut();
        s.pointer = Some(at);
        if extend && s.anchor.is_some() {
            s.head = Some((key, ix));
        } else {
            s.anchor = Some((key, ix));
            s.head = Some((key, ix));
            let shown: Vec<_> = s.shown.iter().map(|(k, _, t)| (*k, t.clone())).collect();
            s.texts = shown.into_iter().collect();
        }
        s.dragging = true;
        // Ctrl+C goes to whatever has the keyboard: keep it in the pane.
        if !s.focus.contains_focused(window, cx) {
            let focus = s.focus.clone();
            drop(s);
            window.focus(&focus, cx);
            s = self.0.borrow_mut();
        }
        cx.notify(s.view);
    }
}

/// The text of the runs from `from` to `to`, cut at both ends.
fn join(texts: &BTreeMap<Key, SharedString>, from: Spot, to: Spot) -> String {
    let mut out = String::new();
    let mut prev: Option<Key> = None;
    for (key, text) in texts.range(from.0..=to.0) {
        if let Some(p) = prev {
            if (p.0, p.1, p.2) != (key.0, key.1, key.2) {
                out.push('\n');
            }
            if p.0 == key.0 && p.1 != key.1 {
                out.push('\n');
            }
        }
        let start = if *key == from.0 { from.1 } else { 0 };
        let end = if *key == to.0 { to.1 } else { text.len() };
        out.push_str(slice(text, start, end));
        prev = Some(*key);
    }
    out
}

/// `text[start..end]`, with both ends moved back onto a character's edge.
fn slice(text: &str, start: usize, end: usize) -> &str {
    let edge = |mut i: usize| {
        i = i.min(text.len());
        while !text.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let (a, b) = (edge(start), edge(end));
    if a < b { &text[a..b] } else { "" }
}

/// The character edge nearest a point, in a laid-out run of text.
fn index_at(layout: &TextLayout, p: Point<Pixels>) -> usize {
    let b = layout.bounds();
    let lh = layout.line_height();
    if p.y < b.top() {
        return 0;
    }
    let mut top = b.top();
    let mut start = 0;
    for line in layout.line_layouts() {
        let bottom = top + line.size(lh).height;
        if p.y < bottom {
            let ix = match line.closest_index_for_position(point(p.x - b.left(), p.y - top), lh) {
                Ok(i) | Err(i) => i,
            };
            return start + ix;
        }
        top = bottom;
        start += line.len() + 1;
    }
    layout.len()
}

/// A run of message text that can be selected: the run itself (with its links and their tips) and
/// its part of the selection drawn under it.
pub struct Selectable {
    id: ElementId,
    inner: InteractiveText,
    layout: TextLayout,
    text: SharedString,
    key: Key,
    sel: Selection,
    color: Hsla,
}

impl Selectable {
    /// `layout` is the one of the `StyledText` inside `inner`, taken before it went in.
    pub fn new(
        id: ElementId,
        inner: InteractiveText,
        layout: TextLayout,
        text: SharedString,
        key: Key,
        sel: Selection,
        color: Hsla,
    ) -> Selectable {
        Selectable { id, inner, layout, text, key, sel, color }
    }
}

impl IntoElement for Selectable {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Selectable {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn a11y_role(&self) -> Option<gpui::accesskit::Role> {
        self.inner.a11y_role()
    }

    fn write_a11y_info(&self, node: &mut gpui::accesskit::Node) {
        self.inner.write_a11y_info(node)
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        self.inner.request_layout(id, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        self.inner.prepaint(id, inspector_id, bounds, state, window, cx)
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut (),
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (key, layout, text, sel) = (self.key, self.layout.clone(), self.text.clone(), self.sel.clone());
        sel.drawn(key, &layout, &text, window);
        if let Some((start, end, more)) = sel.part(key, text.len()) {
            for r in rows(&layout, start, end, more) {
                window.paint_quad(fill(r, self.color));
            }
        }
        // Set before the run draws, so a link's pointing hand, set as it does, wins over it.
        window.set_cursor_style(CursorStyle::IBeam, hitbox);
        let hb = hitbox.clone();
        window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && e.button == MouseButton::Left && e.click_count == 1 && hb.is_hovered(window) {
                let ix = index_at(&layout, e.position);
                sel.press(key, ix, e.position, e.modifiers.shift, window, cx);
            }
        });
        self.inner.paint(id, inspector_id, bounds, state, hitbox, window, cx);
    }
}

/// The selection from `start` to `end` in a run, one rectangle per row it touches; `more` reaches a
/// little past the end, to show the line break is taken too.
fn rows(layout: &TextLayout, start: usize, end: usize, more: bool) -> Vec<Bounds<Pixels>> {
    let b = layout.bounds();
    let lh = layout.line_height();
    let (Some(a), Some(z)) = (layout.position_for_index(start), layout.position_for_index(end)) else {
        return Vec::new();
    };
    let tail = if more { gpui::px(5.) } else { gpui::px(0.) };
    let row = |y: Pixels, x0: Pixels, x1: Pixels| Bounds::from_corners(point(x0, y), point(x1.max(x0), y + lh));
    if a.y == z.y {
        return vec![row(a.y, a.x, z.x + tail)];
    }
    let mut out = vec![row(a.y, a.x, b.right())];
    let mut y = a.y + lh;
    while y < z.y {
        out.push(row(y, b.left(), b.right()));
        y += lh;
    }
    out.push(row(z.y, b.left(), z.x + tail));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_what_is_selected_line_by_line() {
        let texts: BTreeMap<Key, SharedString> = [
            ((1, 0, 0, 0), "hello there"),
            ((1, 0, 0, 1), " and more"),
            ((1, 0, 1, 0), "second line"),
            ((1, 1, 0, 0), "next paragraph"),
            ((2, 0, 0, 0), "another message"),
        ]
        .into_iter()
        .map(|(k, t)| (k, SharedString::from(t)))
        .collect();
        // Half of one run.
        assert_eq!(join(&texts, ((1, 0, 0, 0), 6), ((1, 0, 0, 0), 11)), "there");
        // Runs on one line join as they read; lines break; paragraphs leave a blank line.
        assert_eq!(join(&texts, ((1, 0, 0, 0), 6), ((1, 1, 0, 0), 4)), "there and more\nsecond line\n\nnext");
        // Across messages.
        assert_eq!(join(&texts, ((1, 1, 0, 0), 5), ((2, 0, 0, 0), 7)), "paragraph\nanother");
    }

    #[test]
    fn cuts_on_character_edges() {
        assert_eq!(slice("ação", 0, 2), "a");
        assert_eq!(slice("ação", 1, 99), "ção");
        assert_eq!(slice("abc", 2, 1), "");
    }
}
