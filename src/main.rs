#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! NeoHtop - a modern system monitor built with GPUI Kit.

mod gpu;
mod monitoring;
mod native_icons;
mod preferences;
mod sampling;
mod state;
mod telemetry;
mod ui;

use gpui_kit::assets::{icon_assets, Assets};
use gpui_kit::component::TitleBar;
use gpui_kit::{
    actions, px, size, App, AppContext, AssetSource, Bounds, KeyBinding, Menu, MenuItem,
    SharedString, WindowBounds, WindowOptions,
};
use std::borrow::Cow;
use std::sync::Arc;

actions!(neohtop, [Quit]);

// Lucide icons NeoHtop uses beyond the component defaults.
icon_assets!(
    ExtraIcons,
    [
        Columns3,
        Cpu,
        Funnel,
        Gpu,
        HardDrive,
        Info,
        List,
        ListTree,
        MemoryStick,
        Monitor,
        Network,
        Palette,
        Pause,
        Pin,
        PinOff,
        Play,
        Timer,
        X
    ]
);

struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        match ExtraIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        let mut list = Assets.list(path)?;
        list.extend(ExtraIcons.list(path)?);
        list.sort();
        list.dedup();
        Ok(list)
    }
}

fn main() {
    gpui_kit::application()
        .with_assets(AppAssets)
        .run(|cx: &mut App| {
            gpui_kit::init(cx);

            let (prefs, prefs_error) = preferences::Preferences::open();
            let analytics = prefs.behavior("usageAnalytics").as_bool().unwrap_or(false);
            let prefs_error = prefs_error.or_else(|| telemetry::configure(analytics).err());
            ui::theme::apply(
                prefs
                    .appearance("theme")
                    .as_str()
                    .unwrap_or(ui::theme::DEFAULT_THEME),
                cx,
            );

            cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
            cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
            cx.set_menus(vec![Menu {
                name: "NeoHtop".into(),
                disabled: false,
                items: vec![MenuItem::action("Quit NeoHtop", Quit)],
            }]);
            // A monitor with no window has nothing to show.
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let state = Arc::new(state::AppState::new());
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1280.), px(900.)),
                    cx,
                ))),
                window_min_size: Some(size(px(1120.), px(700.))),
                app_id: Some("com.neohtop.dev".into()),
                ..TitleBar::window_options()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                window.set_window_title("NeoHtop");
                cx.new(|cx| ui::NeoHtop::new(state, prefs, prefs_error, window, cx))
            })
            .expect("failed to open the NeoHtop window");
            cx.activate(true);
        });
}
