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
mod hotkeys;
mod icons;
mod markdown;
mod media;
mod prefs;
mod session;
mod text_field;
mod theme;
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

fn main() {
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
