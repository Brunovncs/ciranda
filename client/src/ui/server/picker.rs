//! "Choose what to share": a screen or a window, with a still of each, and how to send it; or a
//! composition of several, screens, windows and the camera, arranged on one canvas.

use crate::media::camera;
use crate::media::composite::{self, Composition, Layout};
use crate::media::loopback;
use crate::media::screen::{self, Resolution, ScreenChoice, ScreenSource, Sound, SourceKind};
use crate::prefs::{prefs, set_prefs};
use crate::theme::{current, px, radius};
use crate::ui::overlay::{Dismiss, dialog_card};
use crate::widgets::*;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement, ParentElement, Render, RenderImage,
    StatefulInteractiveElement, Styled, StyledImage, Task, Window, div, img,
};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

type OnPick = Rc<dyn Fn(ScreenChoice, &mut Window, &mut App)>;
/// A source to put on a composition already running, with its name and program.
type OnAdd = Rc<dyn Fn(composite::Source, String, String, &mut Window, &mut App)>;
/// A composition already being shared, and the size of the screen it took its shape from.
type Running = (Arc<Composition>, Option<(u32, u32)>);

pub struct Picker {
    focus: FocusHandle,
    kind: SourceKind,
    sources: Option<Vec<ScreenSource>>,
    /// The cameras, for a composition.
    cameras: Vec<camera::Device>,
    /// Each window's program, for a composition to find it again another time.
    apps: HashMap<u64, String>,
    /// Stills by source, filled in as they land; dropped from the window's atlas when replaced.
    stills: HashMap<(SourceKind, u64), Arc<RenderImage>>,
    picked: Option<u64>,
    /// What a composition starts with, bottom first.
    ticked: Vec<composite::Source>,
    /// The saved layout's sources found among those here now, with where they were.
    saved: Vec<(composite::Source, composite::Rect, composite::Crop)>,
    /// The ticks were set from the saved layout or the running composition, once.
    restored: bool,
    /// The composition being shared now, when that is what is being changed.
    composition: Option<Running>,
    resolution: Resolution,
    fps: u32,
    sharp: bool,
    sound: Sound,
    /// Starting a share, or changing one already running.
    changing: bool,
    /// Picking something to take a screenshot of, not to share: no stream settings.
    snapshot: bool,
    on_pick: OnPick,
    /// Picking a source to add to a running composition instead: one click adds it.
    adding: Option<OnAdd>,
    _load: Task<()>,
}

impl EventEmitter<Dismiss> for Picker {}

impl Focusable for Picker {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Picker {
    pub fn new(
        current: Option<ScreenChoice>,
        cx: &mut Context<Self>,
        on_pick: impl Fn(ScreenChoice, &mut Window, &mut App) + 'static,
    ) -> Picker {
        let p = prefs(cx);
        let saved_kind = match p.share_kind.as_str() {
            "window" => SourceKind::Window,
            "composite" => SourceKind::Composite,
            _ => SourceKind::Screen,
        };
        let mut picker = Picker {
            focus: cx.focus_handle(),
            kind: current.as_ref().map(|c| c.source.kind).unwrap_or(saved_kind),
            sources: None,
            cameras: Vec::new(),
            apps: HashMap::new(),
            stills: HashMap::new(),
            picked: current.as_ref().map(|c| c.source.id),
            ticked: Vec::new(),
            saved: Vec::new(),
            restored: false,
            composition: current.as_ref().and_then(|c| c.composition.clone().map(|comp| (comp, c.source.size))),
            // Until one is chosen, what suits this computer's encoder.
            resolution: current.as_ref().map(|c| c.resolution).unwrap_or_else(|| {
                if p.resolution.is_empty() { Resolution::default_for_this_computer() } else { Resolution::from_setting(&p.resolution) }
            }),
            fps: current.as_ref().map(|c| c.fps).unwrap_or(if p.framerate == 0 { 30 } else { p.framerate }),
            sharp: current.as_ref().map(|c| c.sharp).unwrap_or(p.priority != "smooth"),
            sound: current.as_ref().map(|c| c.sound).unwrap_or(match p.share_sound.as_str() {
                "app" => Sound::App,
                "off" => Sound::Off,
                _ => Sound::System,
            }),
            changing: current.is_some(),
            snapshot: false,
            on_pick: Rc::new(on_pick),
            adding: None,
            _load: Task::ready(()),
        };
        cx.on_release(|this: &mut Picker, cx| this.drop_stills(cx)).detach();
        picker.load(cx);
        picker
    }

    /// The same picker, to choose a screen or window to take a picture of.
    pub fn snapshot(cx: &mut Context<Self>, on_pick: impl Fn(ScreenChoice, &mut Window, &mut App) + 'static) -> Picker {
        let mut picker = Picker::new(None, cx, on_pick);
        picker.snapshot = true;
        if picker.kind == SourceKind::Composite {
            picker.kind = SourceKind::Screen;
            picker.load(cx);
        }
        picker
    }

    /// Every screen, window and camera, to put one more on a running composition.
    pub fn adding(cx: &mut Context<Self>, on_add: impl Fn(composite::Source, String, String, &mut Window, &mut App) + 'static) -> Picker {
        let mut picker = Picker::new(None, cx, |_, _, _| {});
        picker.adding = Some(Rc::new(on_add));
        picker.restored = true;
        if picker.kind != SourceKind::Composite {
            picker.kind = SourceKind::Composite;
            picker.load(cx);
        }
        picker
    }

    fn composite(&self) -> bool {
        self.kind == SourceKind::Composite
    }

    /// The sound picked, as far as this computer can share it for `kind`: one application's alone
    /// only for a window, where the system allows it; else the whole computer's; else none.
    fn sound_for(&self, kind: SourceKind) -> Sound {
        let can = loopback::support();
        match self.sound {
            Sound::App if kind == SourceKind::Window && can.app => Sound::App,
            Sound::App | Sound::System if can.system => Sound::System,
            _ => Sound::Off,
        }
    }

    /// The list first, by title; then the stills, in parallel, each drawn as it lands. A
    /// composition lists screens, then windows, then cameras.
    fn load(&mut self, cx: &mut Context<Self>) {
        self.sources = None;
        self.drop_stills(cx);
        let kind = self.kind;
        let composite = self.composite();
        let kinds = if composite { vec![SourceKind::Screen, SourceKind::Window] } else { vec![kind] };
        self._load = cx.spawn(async move |this, cx| {
            let listed = kinds.clone();
            let (list, apps, cameras) = cx
                .background_executor()
                .spawn(async move {
                    let list: Vec<ScreenSource> = listed.iter().flat_map(|k| screen::sources(*k)).collect();
                    let apps: HashMap<u64, String> = if composite {
                        list.iter().filter(|s| s.kind == SourceKind::Window).map(|s| (s.id, composite::window_app(s.id))).collect()
                    } else {
                        HashMap::new()
                    };
                    (list, apps, if composite { camera::devices() } else { Vec::new() })
                })
                .await;
            let ids: Vec<(SourceKind, u64)> = list.iter().map(|s| (s.kind, s.id)).collect();
            let shown = this.update(cx, |this, cx| {
                if this.picked.is_none_or(|id| !list.iter().any(|s| s.id == id)) {
                    this.picked = list.first().map(|s| s.id);
                }
                (this.sources, this.apps, this.cameras) = (Some(list), apps, cameras);
                if composite {
                    this.restore(cx);
                }
                cx.notify();
            });
            if shown.is_err() {
                return;
            }
            // Dropping this task (closed, or another kind picked) drops the receiver, which
            // stops the threads taking stills.
            for k in kinds {
                let stills = screen::stills(k, ids.iter().filter(|(sk, _)| *sk == k).map(|(_, id)| *id).collect());
                while let Ok((id, still)) = stills.recv().await {
                    let Some(still) = still else { continue };
                    let ok = this.update(cx, |this, cx| {
                        if let Some(s) = this.sources.iter_mut().flatten().find(|s| s.kind == k && s.id == id) {
                            s.size = Some(still.size);
                        }
                        if let Some(old) = this.stills.insert((k, id), still.image) {
                            cx.drop_image(old, None);
                        }
                        cx.notify();
                    });
                    if ok.is_err() {
                        return;
                    }
                }
            }
        });
        cx.notify();
    }

    fn drop_stills(&mut self, cx: &mut App) {
        for (_, image) in self.stills.drain() {
            cx.drop_image(image, None);
        }
    }

    fn is_here(&self, s: &composite::Source) -> bool {
        match s {
            composite::Source::Screen(id) | composite::Source::Window(id) => self.sources.iter().flatten().any(|x| x.id == *id),
            composite::Source::Camera(d) => self.cameras.iter().any(|c| &c.id == d),
        }
    }

    /// The ticks a composition starts with, the first time the lists are in: what the running
    /// one shows, or else what the last one did, wherever those sources are found now.
    fn restore(&mut self, cx: &App) {
        if std::mem::replace(&mut self.restored, true) {
            let ticked = std::mem::take(&mut self.ticked);
            self.ticked = ticked.into_iter().filter(|s| self.is_here(s)).collect();
            return;
        }
        if let Some((comp, _)) = &self.composition {
            self.ticked = comp.layout().layers.iter().map(|l| l.source.clone()).collect();
            return;
        }
        let list = self.sources.clone().unwrap_or_default();
        let screens: Vec<ScreenSource> = list.iter().filter(|s| s.kind == SourceKind::Screen).cloned().collect();
        let windows: Vec<(ScreenSource, String)> = list
            .iter()
            .filter(|s| s.kind == SourceKind::Window)
            .map(|s| (s.clone(), self.apps.get(&s.id).cloned().unwrap_or_default()))
            .collect();
        for layer in &prefs(cx).composite_layout {
            let Some((source, _, _)) = composite::find(layer, &screens, &windows, &self.cameras) else { continue };
            if !self.ticked.contains(&source) {
                self.ticked.push(source.clone());
                self.saved.push((source, layer.rect(), layer.crop()));
            }
        }
    }

    /// A source's name and program, as listed.
    fn describe(&self, s: &composite::Source) -> (String, String) {
        match s {
            composite::Source::Screen(id) | composite::Source::Window(id) => {
                let title = self.sources.iter().flatten().find(|x| x.id == *id).map(|x| x.title.clone()).unwrap_or_default();
                (title, self.apps.get(id).cloned().unwrap_or_default())
            }
            composite::Source::Camera(d) => {
                (self.cameras.iter().find(|c| &c.id == d).map(|c| c.name.clone()).unwrap_or(d.clone()), String::new())
            }
        }
    }

    fn tick(&mut self, source: composite::Source, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(add) = self.adding.clone() {
            let (name, app) = self.describe(&source);
            cx.emit(Dismiss);
            add(source, name, app, window, cx);
            return;
        }
        match self.ticked.iter().position(|s| *s == source) {
            Some(i) => {
                self.ticked.remove(i);
            }
            None => self.ticked.push(source),
        }
        cx.notify();
    }

    /// The composition to share: the running one with its sources made the ticked ones, or a
    /// new one, each source where the last one had it or else placed once it is seen.
    fn composition(&self, cx: &App) -> (Arc<Composition>, (u32, u32)) {
        let largest = self
            .sources
            .iter()
            .flatten()
            .filter(|s| s.kind == SourceKind::Screen)
            .filter_map(|s| s.size)
            .max_by_key(|(w, h)| w * h)
            .unwrap_or((1920, 1080));
        if let Some((comp, size)) = &self.composition {
            comp.edit(|l| {
                l.layers.retain(|x| self.ticked.contains(&x.source));
                for s in &self.ticked {
                    if !l.layers.iter().any(|x| x.source == *s) {
                        let (name, app) = self.describe(s);
                        l.add(s.clone(), name, app);
                    }
                }
            });
            return (comp.clone(), size.unwrap_or(largest));
        }
        let mut layout = Layout::new(largest);
        for s in &self.ticked {
            let (name, app) = self.describe(s);
            match self.saved.iter().find(|(x, _, _)| x == s) {
                Some((_, rect, crop)) => layout.add_at(s.clone(), name, app, *rect, *crop),
                None => layout.add(s.clone(), name, app),
            };
        }
        (Composition::new(layout, prefs(cx).camera_background.clone()), largest)
    }

    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let choice = if self.composite() {
            let (comp, size) = self.composition(cx);
            let source =
                ScreenSource { id: 0, kind: SourceKind::Composite, title: tr!("Composition", "Composição").into(), size: Some(size) };
            let sound = self.sound_for(SourceKind::Composite);
            let choice = ScreenChoice {
                source,
                resolution: self.resolution,
                fps: self.fps,
                sharp: self.sharp,
                sound,
                composition: Some(comp.clone()),
            };
            comp.edit(|l| l.canvas = composite::canvas_size(&choice));
            choice
        } else {
            let Some(source) = self.sources.as_ref().and_then(|l| l.iter().find(|s| Some(s.id) == self.picked)).cloned() else { return };
            let sound = self.sound_for(source.kind);
            ScreenChoice { source, resolution: self.resolution, fps: self.fps, sharp: self.sharp, sound, composition: None }
        };
        let (res, fps, sharp, kind, sound) = (self.resolution, self.fps, self.sharp, self.kind, self.sound);
        if self.snapshot {
            cx.emit(Dismiss);
            (self.on_pick)(choice, window, cx);
            return;
        }
        set_prefs(cx, |p| {
            p.resolution = res.setting().into();
            p.framerate = fps;
            p.priority = if sharp { "sharp" } else { "smooth" }.into();
            p.share_kind = match kind {
                SourceKind::Window => "window",
                SourceKind::Composite => "composite",
                SourceKind::Screen => "screen",
            }
            .into();
            p.share_sound = match sound {
                Sound::System => "system",
                Sound::App => "app",
                Sound::Off => "off",
            }
            .into();
        });
        cx.emit(Dismiss);
        (self.on_pick)(choice, window, cx);
    }

    /// A source's card: its still (or an icon) and its name, marked when picked; in a composition
    /// its place in the stack.
    fn card(&self, key: (SourceKind, u64), title: String, size: Option<(u32, u32)>, glyph: &'static str, on: Option<usize>) -> gpui::Div {
        let t = current();
        let hover = t.stroke_strong;
        let (picked, order) = (on.is_some(), on.filter(|_| self.composite() && self.adding.is_none()));
        div()
            .relative()
            .w(px(214.))
            .flex()
            .flex_col()
            .rounded(px(radius::CARD))
            .overflow_hidden()
            .bg(t.layer)
            .border_2()
            .border_color(if picked { t.accent } else { t.stroke })
            .cursor_pointer()
            .when(!picked, |d| d.hover(move |s| s.border_color(hover)))
            .child(div().h(px(120.)).bg(t.stage).flex().items_center().justify_center().child(match self.stills.get(&key) {
                Some(i) => img(i.clone()).size_full().object_fit(gpui::ObjectFit::Contain).into_any_element(),
                None => icon(glyph, 24., t.text3).into_any_element(),
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(10.))
                    .py(px(8.))
                    .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(13.)).font_weight(gpui::FontWeight::MEDIUM).child(title))
                    .when_some(size, |d, (w, h)| d.child(mono(format!("{w}×{h}"), t.text3))),
            )
            .when_some(order, |d, i| {
                d.child(
                    div()
                        .absolute()
                        .top(px(6.))
                        .left(px(6.))
                        .size(px(22.))
                        .rounded_full()
                        .bg(t.accent)
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(12.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(t.on_accent)
                        .child(format!("{}", i + 1)),
                )
            })
    }
}

impl Render for Picker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = current();
        let composite = self.composite();
        let mut grid = div().id("sources").flex().flex_wrap().gap(px(10.)).max_h(px(330.)).overflow_y_scroll();
        match &self.sources {
            None => {
                grid = grid.child(
                    div()
                        .w_full()
                        .py(px(60.))
                        .flex()
                        .justify_center()
                        .child(caption(tr!("Looking for what you can share…", "Procurando o que dá para compartilhar…"), t.text3)),
                );
            }
            Some(list) if list.is_empty() && (!composite || self.cameras.is_empty()) => {
                grid = grid.child(div().w_full().py(px(60.)).flex().justify_center().child(caption(
                    if self.kind == SourceKind::Window {
                        tr!(
                            "No windows to share. Open the app you want to show first.",
                            "Nenhuma janela para compartilhar. Abra primeiro o app que você quer mostrar."
                        )
                    } else {
                        tr!("No screens found.", "Nenhuma tela encontrada.")
                    },
                    t.text3,
                )));
            }
            Some(list) => {
                for s in list.clone() {
                    let id = s.id;
                    let glyph = if s.kind == SourceKind::Screen { "monitor" } else { "image" };
                    if composite {
                        let source =
                            if s.kind == SourceKind::Screen { composite::Source::Screen(id) } else { composite::Source::Window(id) };
                        let on = self.ticked.iter().position(|x| *x == source);
                        grid = grid.child(
                            self.card((s.kind, id), s.title.clone(), s.size, glyph, on)
                                .id(("source", id))
                                .on_click(cx.listener(move |this, _, window, cx| this.tick(source.clone(), window, cx))),
                        );
                        continue;
                    }
                    let on = (Some(id) == self.picked).then_some(0);
                    grid = grid.child(self.card((s.kind, id), s.title.clone(), s.size, glyph, on).id(("source", id)).on_click(
                        cx.listener(move |this, e: &gpui::ClickEvent, window, cx| {
                            this.picked = Some(id);
                            if e.click_count() == 2 {
                                this.start(window, cx);
                            }
                            cx.notify();
                        }),
                    ));
                }
                if composite {
                    for (i, c) in self.cameras.clone().into_iter().enumerate() {
                        let source = composite::Source::Camera(c.id.clone());
                        let on = self.ticked.iter().position(|x| *x == source);
                        grid = grid.child(
                            self.card((SourceKind::Composite, i as u64), c.name.clone(), None, "camera", on)
                                .id(("camera-source", i))
                                .on_click(cx.listener(move |this, _, window, cx| this.tick(source.clone(), window, cx))),
                        );
                    }
                }
            }
        }
        // Only the sounds this computer can share: no silent track offered as "Computer sound".
        let can = loopback::support();
        let one_app = self.kind == SourceKind::Window && can.app;
        let mut sound_choices: Vec<(Sound, gpui::SharedString)> = Vec::new();
        if one_app {
            sound_choices.push((Sound::App, tr!("This app only", "Só este app").into()));
        }
        if can.system {
            let name = if one_app { tr!("Everything", "Tudo") } else { tr!("Computer sound", "Som do computador") };
            sound_choices.push((Sound::System, name.into()));
        }
        sound_choices.push((Sound::Off, tr!("No sound", "Sem som").into()));
        let sound_offered = sound_choices.len() > 1;
        let sound = self.sound_for(self.kind);
        let note = match sound {
            Sound::System if can.without_call => tr!(
                "Viewers hear what your computer plays, but not the call.",
                "Quem assiste ouve o que o computador toca, mas não a chamada."
            ),
            Sound::System => tr!(
                "Viewers hear everything your computer plays, the call included.",
                "Quem assiste ouve tudo o que o computador toca, inclusive a chamada."
            ),
            Sound::App => tr!("Viewers hear only this app.", "Quem assiste ouve só este app."),
            Sound::Off if !sound_offered => {
                tr!("Sound can't be shared from this computer.", "Não dá para compartilhar o som deste computador.")
            }
            Sound::Off => tr!("Viewers see the picture without sound.", "Quem assiste vê a imagem sem som."),
        };
        let adding = self.adding.is_some();
        let settings = !self.snapshot && !adding;
        let field = |name: &'static str, control: gpui::Div| div().flex().flex_col().gap(px(6.)).child(label(name, &t)).child(control);
        let mut kinds = vec![(SourceKind::Screen, tr!("Screens", "Telas").into()), (SourceKind::Window, tr!("Windows", "Janelas").into())];
        if !self.snapshot {
            kinds.push((SourceKind::Composite, tr!("Composite", "Composição").into()));
        }
        dialog_card(&t, 720.)
            .track_focus(&self.focus)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .p(px(22.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(title(
                                if adding {
                                    tr!("Add a source", "Adicionar uma fonte")
                                } else if self.snapshot {
                                    tr!("Send a screenshot", "Enviar uma captura de tela")
                                } else if self.changing {
                                    tr!("Change what you share", "Trocar o que você compartilha")
                                } else {
                                    tr!("Choose what to share", "Escolha o que compartilhar")
                                },
                                t.text,
                            ))
                            .child(caption(
                                if adding {
                                    tr!("It goes on top of your composition.", "Ela entra por cima da sua composição.")
                                } else if self.snapshot {
                                    tr!(
                                        "It goes into your message at full size, for you to send or drop.",
                                        "Ela entra na sua mensagem em tamanho real, para você enviar ou descartar."
                                    )
                                } else if composite {
                                    tr!(
                                        "Pick any number to start with, or none: once live you can add, move, resize and crop them.",
                                        "Escolha quantas quiser para começar, ou nenhuma: ao vivo dá para adicionar, mover, redimensionar e cortar."
                                    )
                                } else {
                                    tr!("People in the channel see it as a tile.", "Quem está no canal vê isso num quadro.")
                                },
                                t.text2,
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .items_center()
                            .when(!adding, |d| {
                                d.child(div().flex_1().child(segmented("kind", kinds, self.kind, &t, cx, |this, k, _, cx| {
                                    this.kind = k;
                                    this.load(cx);
                                })))
                            })
                            .when(adding, |d| d.child(div().flex_1()))
                            .child(
                                button("refresh", tr!("Refresh", "Atualizar"), Kind::Standard, &t)
                                    .on_click(cx.listener(|this, _, _, cx| this.load(cx))),
                            ),
                    )
                    .child(grid)
                    .when(settings, |d| {
                        d.child(
                            div()
                                .flex()
                                .gap(px(14.))
                                .child(div().flex_1().child(field(
                                    tr!("Resolution", "Resolução"),
                                    segmented(
                                        "res",
                                        vec![
                                            (Resolution::P480, "480p".into()),
                                            (Resolution::P720, "720p".into()),
                                            (Resolution::P1080, "1080p".into()),
                                            (Resolution::Native, tr!("Native", "Nativa").into()),
                                        ],
                                        self.resolution,
                                        &t,
                                        cx,
                                        |this, r, _, cx| {
                                            this.resolution = r;
                                            cx.notify();
                                        },
                                    ),
                                )))
                                .child(div().w(px(230.)).child(field(
                                    tr!("Frame rate", "Quadros por segundo"),
                                    segmented(
                                        "fps",
                                        vec![(30, "30".into()), (60, "60".into()), (120, "120".into())],
                                        self.fps,
                                        &t,
                                        cx,
                                        |this, f, _, cx| {
                                            this.fps = f;
                                            cx.notify();
                                        },
                                    ),
                                ))),
                        )
                        .child(
                            div()
                                .flex()
                                .gap(px(14.))
                                .child(div().flex_1().child(field(
                                    tr!("When the connection is tight", "Se a conexão piorar"),
                                    segmented(
                                        "priority",
                                        vec![
                                            (true, tr!("Keep it sharp", "Priorizar a nitidez").into()),
                                            (false, tr!("Keep it smooth", "Priorizar a fluidez").into()),
                                        ],
                                        self.sharp,
                                        &t,
                                        cx,
                                        |this, s, _, cx| {
                                            this.sharp = s;
                                            cx.notify();
                                        },
                                    ),
                                )))
                                .when(sound_offered, |d| {
                                    d.child(div().flex_1().child(field(
                                        tr!("Sound", "Som"),
                                        segmented("sound", sound_choices, sound, &t, cx, |this, s, _, cx| {
                                            this.sound = s;
                                            cx.notify();
                                        }),
                                    )))
                                }),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(22.))
                    .py(px(14.))
                    .bg(t.layer)
                    .border_t_1()
                    .border_color(t.stroke)
                    .child(div().flex_1().when(settings, |d| d.child(caption(note, t.text2))))
                    .child(
                        button("picker-cancel", tr!("Cancel", "Cancelar"), Kind::Standard, &t)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Dismiss))),
                    )
                    .when(!adding, |d| {
                        d.child(
                            button(
                                "picker-start",
                                if self.snapshot {
                                    tr!("Capture", "Capturar")
                                } else if self.changing {
                                    tr!("Use this", "Usar")
                                } else {
                                    tr!("Start sharing", "Começar a compartilhar")
                                },
                                Kind::Primary,
                                &t,
                            )
                            .when(self.picked.is_none() && !composite, |b| b.opacity(0.5))
                            .on_click(cx.listener(|this, _, window, cx| this.start(window, cx))),
                        )
                    }),
            )
    }
}
