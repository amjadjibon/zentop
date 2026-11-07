#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Zentop - a modern system monitor built with GPUI Kit.

mod gpu;
mod monitoring;
mod native_icons;
mod preferences;
mod sampling;
mod state;
mod ui;

use gpui_kit::assets::{icon_assets, Assets};
use gpui_kit::{App, AssetSource, SharedString};
use std::borrow::Cow;

// Lucide icons Zentop uses beyond the component defaults.
icon_assets!(
    ExtraIcons,
    [
        AppWindow,
        ArrowDown,
        ArrowUp,
        ArrowUpRight,
        ChevronsUpDown,
        ChevronLeft,
        ChevronRight,
        ChevronsLeft,
        ChevronsRight,
        CircleQuestionMark,
        PanelsTopLeft,
        Check,
        Columns3,
        Cpu,
        Ellipsis,
        Funnel,
        Gpu,
        HardDrive,
        Info,
        List,
        ListTree,
        MemoryStick,
        Monitor,
        Server,
        Network,
        Palette,
        Pause,
        Pin,
        PinOff,
        Play,
        Power,
        Settings,
        SlidersHorizontal,
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
    let app = gpui_kit::application().with_assets(AppAssets);
    // Clicking the Dock icon (or relaunching) brings the monitor back.
    app.on_reopen(ui::open_main);
    app.run(|cx: &mut App| {
        gpui_kit::init(cx);
        ui::init(cx);
    });
}
