//! Zentop windows: the main monitor, Settings, About and the menu-bar overview.
mod about;
mod companion;
mod details;
mod main_window;
mod monitor;
mod process_view;
mod resource_details;
mod settings;
mod stats;
mod table;
pub mod theme;
mod tray;

use gpui_kit::component::TitleBar;
use gpui_kit::{
    actions, px, size, AnyWindowHandle, App, AppContext, Bounds, Global, KeyBinding, Menu,
    MenuItem, OsAction, Pixels, Size, WeakEntity, Window, WindowBounds, WindowOptions,
};
use main_window::Zentop;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

actions!(
    zentop,
    [
        Quit,
        OpenSettings,
        OpenAbout,
        ShowOverview,
        ShowMain,
        ViewList,
        ViewTree,
        CloseWindow,
        Minimize,
    ]
);

pub static APP_ICON: LazyLock<Arc<gpui_kit::Image>> = LazyLock::new(|| {
    Arc::new(gpui_kit::Image::from_bytes(
        gpui_kit::ImageFormat::Png,
        include_bytes!("../../assets/icons/128x128.png").to_vec(),
    ))
});

pub const REPOSITORY: &str = "https://github.com/Abdenasser/neohtop";

/// Open windows, so each kind is reused instead of duplicated.
#[derive(Default)]
struct Windows {
    main: Option<(AnyWindowHandle, WeakEntity<Zentop>)>,
    settings: Option<AnyWindowHandle>,
    about: Option<AnyWindowHandle>,
    overview: Option<AnyWindowHandle>,
    /// A tray click that dismissed the overview must not immediately reopen it.
    overview_closed_at: Option<Instant>,
}
impl Global for Windows {}

pub fn init(cx: &mut App) {
    cx.set_global(Windows::default());
    let monitor = monitor::init(cx);
    // Keep the tray in step with the menu-bar preference.
    cx.observe(&monitor, |_, cx| tray::sync(cx)).detach();
    tray::init(cx);

    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("ctrl-q", Quit, None),
        KeyBinding::new("ctrl-,", OpenSettings, None),
    ]);
    cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
    cx.on_action(|_: &OpenSettings, cx: &mut App| open_settings(cx));
    cx.on_action(|_: &OpenAbout, cx: &mut App| open_about(cx));
    cx.on_action(|_: &ShowOverview, cx: &mut App| companion::show(None, cx));
    cx.on_action(|_: &ShowMain, cx: &mut App| open_main(cx));
    cx.on_action(|_: &ViewList, cx: &mut App| set_view(false, cx));
    cx.on_action(|_: &ViewTree, cx: &mut App| set_view(true, cx));
    cx.on_action(|_: &CloseWindow, cx: &mut App| {
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_, window, _| window.remove_window());
        }
    });
    cx.on_action(|_: &Minimize, cx: &mut App| {
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_, window, _| window.minimize_window());
        }
    });
    cx.set_menus(vec![
        Menu {
            name: "Zentop".into(),
            disabled: false,
            items: vec![
                MenuItem::action("About Zentop", OpenAbout),
                MenuItem::separator(),
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::separator(),
                MenuItem::action("Quit Zentop", Quit),
            ],
        },
        Menu {
            name: "Edit".into(),
            disabled: false,
            items: vec![
                MenuItem::os_action("Undo", gpui_kit::component::input::Undo, OsAction::Undo),
                MenuItem::os_action("Redo", gpui_kit::component::input::Redo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action("Cut", gpui_kit::component::input::Cut, OsAction::Cut),
                MenuItem::os_action("Copy", gpui_kit::component::input::Copy, OsAction::Copy),
                MenuItem::os_action("Paste", gpui_kit::component::input::Paste, OsAction::Paste),
                MenuItem::os_action(
                    "Select All",
                    gpui_kit::component::input::SelectAll,
                    OsAction::SelectAll,
                ),
            ],
        },
        Menu {
            name: "View".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Process List", ViewList),
                MenuItem::action("Process Tree", ViewTree),
                MenuItem::separator(),
                MenuItem::action("Menu-bar Overview", ShowOverview),
            ],
        },
        Menu {
            name: "Window".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Show Zentop", ShowMain),
                MenuItem::action("Minimize", Minimize),
                MenuItem::action("Close Window", CloseWindow),
            ],
        },
    ]);

    // Without the menu-bar companion there is nothing left to monitor from.
    cx.on_window_closed(|cx, _| {
        if cx.windows().is_empty() && !tray::keeps_running(cx) {
            cx.quit();
        }
    })
    .detach();

    open_main(cx);
    cx.activate(true);
}

fn focus(handle: Option<AnyWindowHandle>, cx: &mut App) -> bool {
    handle.is_some_and(|handle| {
        handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    })
}

fn window_options(size: Size<Pixels>, min: Option<Size<Pixels>>, cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size, cx))),
        window_min_size: min,
        app_id: Some("com.zentop.app".into()),
        ..TitleBar::window_options()
    }
}

pub fn open_main(cx: &mut App) {
    companion::close(cx);
    let existing = cx
        .global::<Windows>()
        .main
        .as_ref()
        .map(|(handle, _)| *handle);
    if focus(existing, cx) {
        cx.activate(true);
        return;
    }
    let options = window_options(
        size(px(1280.), px(900.)),
        Some(size(px(1120.), px(700.))),
        cx,
    );
    match gpui_kit::open_window(options, cx, |window, cx| {
        window.set_window_title("Zentop");
        cx.new(|cx| Zentop::new(window, cx))
    }) {
        Ok((handle, view)) => cx.global_mut::<Windows>().main = Some((handle, view.downgrade())),
        Err(error) => eprintln!("Zentop: could not open the main window: {error}"),
    }
    cx.activate(true);
}

/// Runs `f` in the main window, opening it first if needed.
fn with_main(cx: &mut App, f: impl FnOnce(&mut Window, &mut App) + 'static) {
    open_main(cx);
    if let Some((handle, _)) = cx.global::<Windows>().main.clone() {
        let _ = handle.update(cx, |_, window, cx| f(window, cx));
    }
}

pub fn open_settings(cx: &mut App) {
    companion::close(cx);
    if focus(cx.global::<Windows>().settings, cx) {
        cx.activate(true);
        return;
    }
    let options = window_options(size(px(760.), px(600.)), Some(size(px(640.), px(480.))), cx);
    if let Ok((handle, _)) = gpui_kit::open_window(options, cx, |window, cx| {
        window.set_window_title("Zentop Settings");
        cx.new(|cx| settings::Settings::new(window, cx))
    }) {
        cx.global_mut::<Windows>().settings = Some(handle);
    }
    cx.activate(true);
}

pub fn open_about(cx: &mut App) {
    companion::close(cx);
    if focus(cx.global::<Windows>().about, cx) {
        cx.activate(true);
        return;
    }
    let mut options = window_options(size(px(420.), px(440.)), None, cx);
    options.is_resizable = false;
    if let Ok((handle, _)) = gpui_kit::open_window(options, cx, |window, cx| {
        window.set_window_title("About Zentop");
        cx.new(|_| about::About)
    }) {
        cx.global_mut::<Windows>().about = Some(handle);
    }
    cx.activate(true);
}

fn set_view(tree: bool, cx: &mut App) {
    let view = if tree { "tree" } else { "flat" };
    monitor::global(cx).update(cx, |monitor, cx| {
        monitor.save(serde_json::json!({"behavior": {"processView": view}}), cx)
    });
    open_main(cx);
}

/// Opens a process's details in the main window, from the overview.
fn show_process(id: process_view::Identity, cx: &mut App) {
    with_main(cx, move |window, cx| details::open(id, window, cx));
}

fn recently_closed_overview(cx: &App) -> bool {
    cx.global::<Windows>()
        .overview_closed_at
        .is_some_and(|t| t.elapsed() < Duration::from_millis(250))
}
