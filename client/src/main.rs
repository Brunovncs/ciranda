#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// `ApiError` carries the server's reply for callers that branch on it; boxing it everywhere
// would only add noise.
#![allow(clippy::result_large_err)]

//! Ciranda's desktop client: voice channels with cameras and screen sharing, text channels, and
//! the server's people, drawn with GPUI.

#[macro_use]
mod i18n;
mod core;
mod dm;
mod elsewhere;
mod emoji;
mod files;
// The notification-area icon, game audio and replays are Windows only so far, and so is telling
// which hotkeys other programs hold; elsewhere their platform halves are stubs.
#[cfg_attr(not(windows), allow(dead_code))]
mod hotkeys;
mod icons;
mod markdown;
mod media;
mod prefs;
mod session;
mod text_field;
mod theme;
#[cfg_attr(not(windows), allow(dead_code))]
mod tray;
mod ui;
mod widgets;

use gpui::{AppContext, Bounds, QuitMode, TitlebarOptions, WindowBackgroundAppearance, WindowBounds, WindowOptions, size};

/// Every video frame shown is a new picture of several megabytes (GPUI's images own their
/// pixels). Windows' heap hands blocks that size straight back to the system, so each one costs
/// fresh pages; mimalloc keeps them, about three times faster per 1080p frame.
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// 1180×760, or less on a small screen, so the whole window and its title bar start in view
/// with room around them.
fn first_size(cx: &gpui::App) -> gpui::Size<gpui::Pixels> {
    let want = size(gpui::px(1180.), gpui::px(760.));
    let Some(display) = cx.primary_display() else { return want };
    let room = display.visible_bounds().size;
    size(want.width.min(room.width * 0.85), want.height.min(room.height * 0.8))
}

/// Where the window was when Ciranda last ran, while its title bar is still on a screen (one
/// unplugged since would leave it out of reach); else centred.
fn first_bounds(cx: &gpui::App) -> WindowBounds {
    if let Some(p) = prefs::prefs(cx).window.filter(|p| p.width >= 640. && p.height >= 480.) {
        let b = Bounds::new(gpui::point(gpui::px(p.x), gpui::px(p.y)), size(gpui::px(p.width), gpui::px(p.height)));
        let title_bar = Bounds::new(b.origin, size(b.size.width.min(gpui::px(240.)), gpui::px(32.)));
        if cx.displays().iter().any(|d| d.bounds().intersects(&title_bar)) {
            return if p.maximized { WindowBounds::Maximized(b) } else { WindowBounds::Windowed(b) };
        }
    }
    WindowBounds::Windowed(Bounds::centered(None, first_size(cx), cx))
}

/// `--self-test`: the release workflow runs the exe it is about to publish with this, so one that
/// builds but cannot start fails there instead of on somebody's machine. It starts everything that
/// needs no window, device, network or settings (libwebrtc, the background model, the private
/// conversations' crypto, and GPUI with its fonts and icons), prints the version and exits 0.
/// GPUI's shaders are only loaded with a window, so they are not covered.
fn self_test() -> ! {
    fn fail(what: &str, why: impl std::fmt::Display) -> ! {
        eprintln!("self-test: {what}: {why}");
        std::process::exit(1)
    }
    // Off the main thread, as in the running app: making libwebrtc's factory puts its thread in
    // COM's multithreaded apartment (the Media Foundation encoder), and GPUI's OleInitialize
    // then fails on it.
    let checks = std::thread::spawn(|| {
        use crate::core::crypto;
        let (a, b) = (crypto::Identity::generate(), crypto::Identity::generate());
        let sealed = crypto::pair_key(&a, 1, &b.public(), 2, 3).unwrap_or_else(|| fail("crypto", "no pair key")).seal("aad", b"hello");
        match crypto::pair_key(&b, 2, &a.public(), 1, 3).and_then(|k| k.open("aad", &sealed)) {
            Some(plain) if plain == b"hello" => {}
            _ => fail("crypto", "a sealed message did not open on the other side"),
        }
        if let Err(e) = media::background::Segmenter::load() {
            fail("background model", e);
        }
        media::rtc::factory();
    });
    if checks.join().is_err() {
        fail("checks", "panicked");
    }
    gpui_platform::application().with_assets(icons::Assets).run(|cx: &mut gpui::App| {
        if let Err(e) = cx.text_system().add_fonts(theme::fonts()) {
            fail("fonts", e);
        }
        if !cx.text_system().all_font_names().iter().any(|f| f == "Figtree") {
            fail("fonts", "Figtree is not among the loaded fonts");
        }
        if !matches!(cx.asset_source().load("icons/mic.svg"), Ok(Some(svg)) if svg.starts_with(b"<svg")) {
            fail("icons", "icons/mic.svg did not load");
        }
        println!("ciranda {} self-test passed", env!("CARGO_PKG_VERSION"));
        std::process::exit(0)
    });
    fail("gpui", "the application returned without running")
}

#[cfg(target_os = "macos")]
gpui::actions!(ciranda, [Quit, Hide]);

/// The menu bar's application menu, which a Mac app is expected to have: without it Cmd+Q does
/// nothing. Quitting is the same as from the tray icon's menu on Windows.
#[cfg(target_os = "macos")]
fn app_menu(cx: &mut gpui::App) {
    use gpui::{KeyBinding, Menu, MenuItem};
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.bind_keys([KeyBinding::new("cmd-q", Quit, None), KeyBinding::new("cmd-h", Hide, None)]);
    cx.set_menus([Menu::new("Ciranda").items([
        MenuItem::action(tr!("Hide Ciranda", "Ocultar Ciranda"), Hide),
        MenuItem::separator(),
        MenuItem::action(tr!("Quit Ciranda", "Encerrar Ciranda"), Quit),
    ])]);
}

/// X11 (XWayland on a Wayland desktop) unless `CIRANDA_WAYLAND` is set: GNOME's compositor
/// leaves window decorations to the program, and GPUI draws none, so on Wayland there the window
/// would have no title bar to move or close it by.
///
/// `WAYLAND_DISPLAY` is emptied, not removed. GPUI takes Wayland only for a non-empty one, but
/// libwebrtc shares the screen through the desktop portal (PipeWire) when the session is Wayland
/// and the variable merely exists; removed, it fell back to its X11 capturer, which on Wayland
/// sees only XWayland's windows and a black screen. Nothing else Ciranda uses connects to Wayland.
/// Set once here and never touched again, so no thread ever reads it while it changes.
#[cfg(target_os = "linux")]
fn prefer_x11() {
    if std::env::var_os("DISPLAY").is_some_and(|d| !d.is_empty())
        && std::env::var_os("CIRANDA_WAYLAND").is_none()
        && std::env::var_os("WAYLAND_DISPLAY").is_some()
    {
        // SAFETY: first thing in main, before any thread that could read the environment.
        unsafe { std::env::set_var("WAYLAND_DISPLAY", "") };
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    prefer_x11();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("ciranda=info,warn")).init();
    let smoke = std::env::args().any(|a| a == "--smoke");
    // The installer's way to close a running Ciranda before it replaces or removes the program.
    if std::env::args().any(|a| a == "--quit") {
        let gone = tray::quit_running(&core::settings::data_dir(), std::time::Duration::from_secs(15));
        std::process::exit(if gone { 0 } else { 1 });
    }
    // Before anything below, which writes to the registry and the data folder.
    if std::env::args().any(|a| a == "--self-test") {
        self_test();
    }
    core::install::note_version();
    let relaunched = core::update::relaunched();
    if let Some(legacy) = core::settings::legacy_dir() {
        let Some(_harmony) = tray::Instance::claim_legacy(&legacy, relaunched) else {
            log::info!("Harmony is running on {}: its window was asked to show", legacy.display());
            return;
        };
        core::settings::adopt_legacy_dir(&legacy);
    }
    let Some(instance) = tray::Instance::claim(&core::settings::data_dir(), relaunched) else {
        log::info!("Ciranda is already running: its window was asked to show");
        return;
    };
    // An update on trial counts this start before anything that could crash it (reading the
    // settings included), and one that kept dying as it started is undone here, short of the code
    // that killed it. After the instance is claimed: a second start that only shows the window is
    // no start of the new version.
    if let Ok(exe) = std::env::current_exe()
        && core::update::start(&exe) == core::update::Start::RollBack
    {
        log::warn!("this version died as it started, again: putting the previous one back");
        match core::update::roll_back(&exe) {
            Ok(()) => return,
            // Kept from the rename (a scanner, a locked folder): giving up on it, this version
            // starts after all rather than refusing to start every time from now on.
            Err(e) => {
                log::error!("the previous version could not be put back: {e}");
                core::update::survived(&exe);
            }
        }
    }
    core::settings::make_private(&core::settings::data_dir());
    // Before the settings are read, which opens what it seals.
    core::secret::init();
    let store = core::settings::Store::load();
    i18n::set(i18n::resolve(&store.values.language));

    // Closing the window never closes it (see `ui::tray`): it hides, or Ciranda quits with it
    // still open so a call is left properly. Quitting when it is gone covers the platforms
    // where it can be closed some other way.
    gpui_platform::application().with_assets(icons::Assets).with_quit_mode(QuitMode::LastWindowClosed).run(move |cx: &mut gpui::App| {
        if let Err(e) = cx.text_system().add_fonts(theme::fonts()) {
            log::warn!("fonts did not load: {e}");
        }
        text_field::bind_keys(cx);
        #[cfg(target_os = "macos")]
        app_menu(cx);
        // The name notifications go out under.
        #[cfg(target_os = "linux")]
        cx.set_app_identity("ciranda", "Ciranda");
        cx.set_global(prefs::Prefs::new(store));
        prefs::init(cx);
        ui::updates::init(cx);
        ui::hotkeys::init(cx);

        let options = WindowOptions {
            window_bounds: Some(first_bounds(cx)),
            titlebar: Some(TitlebarOptions { title: Some("Ciranda".into()), ..Default::default() }),
            app_id: Some("ciranda".into()),
            window_min_size: Some(size(gpui::px(640.), gpui::px(480.))),
            window_background: WindowBackgroundAppearance::Opaque,
            ..Default::default()
        };
        let window = cx
            .open_window(options, |window, cx| {
                let root = cx.new(|cx| ui::Root::new(window, cx));
                cx.set_global(ui::overlay::RootHandle(root.downgrade()));
                root
            })
            .expect("open the window");
        ui::tray::init(window.into(), &instance, cx);
        cx.activate(true);

        if smoke {
            cx.spawn(async move |cx| {
                cx.background_executor().timer(std::time::Duration::from_millis(1500)).await;
                println!("smoke: window opened");
                cx.update(|cx| cx.quit());
            })
            .detach();
        }
    });
}
