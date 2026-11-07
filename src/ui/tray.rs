//! The menu-bar (system tray) icon. GPUI has no tray API, so this uses
//! `tray-icon`; its events arrive on channels that a GPUI task drains.
//!
//! macOS and Windows own the tray on the main thread. Linux needs a GTK main
//! loop, which runs on a dedicated thread that only ever touches the tray.
use super::companion::{self, Anchor};
use super::monitor;
use gpui_kit::{App, Global};
use std::time::Duration;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

const SHOW_OVERVIEW: &str = "overview";
const OPEN_MAIN: &str = "open-main";
const QUIT: &str = "quit";

#[derive(Default)]
struct Tray {
    enabled: bool,
    #[cfg(not(target_os = "linux"))]
    icon: Option<tray_icon::TrayIcon>,
    #[cfg(target_os = "linux")]
    linux: Option<std::sync::mpsc::Sender<bool>>,
}
impl Global for Tray {}

/// Whether closing the last window should leave Zentop running in the tray.
pub fn keeps_running(cx: &App) -> bool {
    let m = monitor::global(cx).read(cx);
    cx.global::<Tray>().enabled && m.behavior_bool("keepRunning")
}

pub fn init(cx: &mut App) {
    cx.set_global(Tray::default());
    sync(cx);
    cx.spawn(async move |cx| loop {
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::Click {
                rect,
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let anchor = Anchor {
                    x: rect.position.x,
                    y: rect.position.y,
                    width: rect.size.width as f64,
                    height: rect.size.height as f64,
                };
                cx.update(|cx| companion::toggle(Some(anchor), cx));
            }
        }
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            cx.update(|cx| match event.id.as_ref() {
                SHOW_OVERVIEW => companion::show(None, cx),
                OPEN_MAIN => super::open_main(cx),
                QUIT => cx.quit(),
                _ => {}
            });
        }
        cx.background_executor()
            .timer(Duration::from_millis(80))
            .await;
    })
    .detach();
}

/// Creates or removes the tray icon to match the menu-bar preference.
pub fn sync(cx: &mut App) {
    let wanted = monitor::global(cx).read(cx).behavior_bool("menuBarEnabled");
    if cx.global::<Tray>().enabled == wanted {
        return;
    }
    let enabled = set_enabled(wanted, cx);
    cx.global_mut::<Tray>().enabled = enabled;
}

fn build() -> Result<tray_icon::TrayIcon, String> {
    let menu = Menu::new();
    menu.append_items(&[
        &MenuItem::with_id(SHOW_OVERVIEW, "Show Overview", true, None),
        &MenuItem::with_id(OPEN_MAIN, "Open Zentop", true, None),
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id(QUIT, "Quit Zentop", true, None),
    ])
    .map_err(|e| e.to_string())?;
    TrayIconBuilder::new()
        .with_icon(ghost()?)
        .with_icon_as_template(true)
        .with_tooltip("Zentop — system overview")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
        .map_err(|e| e.to_string())
}

#[cfg(not(target_os = "linux"))]
fn set_enabled(enabled: bool, cx: &mut App) -> bool {
    let tray = cx.global_mut::<Tray>();
    if !enabled {
        tray.icon = None;
        return false;
    }
    match build() {
        Ok(icon) => {
            tray.icon = Some(icon);
            true
        }
        Err(error) => {
            eprintln!("Zentop: menu-bar icon unavailable: {error}");
            false
        }
    }
}

#[cfg(target_os = "linux")]
fn set_enabled(enabled: bool, cx: &mut App) -> bool {
    let tray = cx.global_mut::<Tray>();
    if tray.linux.is_none() {
        let (sender, receiver) = std::sync::mpsc::channel::<bool>();
        let spawned = std::thread::Builder::new()
            .name("zentop-tray".into())
            .spawn(move || {
                if gtk::init().is_err() {
                    eprintln!("Zentop: GTK is unavailable, so there is no tray icon.");
                    return;
                }
                let mut icon: Option<tray_icon::TrayIcon> = None;
                gtk::glib::timeout_add_local(Duration::from_millis(100), move || {
                    while let Ok(enabled) = receiver.try_recv() {
                        icon = if enabled {
                            build()
                                .map_err(|e| eprintln!("Zentop: tray unavailable: {e}"))
                                .ok()
                        } else {
                            None
                        };
                    }
                    gtk::glib::ControlFlow::Continue
                });
                gtk::main();
            });
        if spawned.is_err() {
            return false;
        }
        tray.linux = Some(sender);
    }
    tray.linux.as_ref().is_some_and(|s| s.send(enabled).is_ok()) && enabled
}

/// A monochrome pixel ghost matching the app's silhouette. macOS treats it as a
/// template, so the system picks menu-bar contrast in light and dark mode.
fn ghost() -> Result<Icon, String> {
    let mut rgba = vec![0u8; 32 * 32 * 4];
    for y in 0..32i32 {
        for x in 0..32i32 {
            let head = (x - 16).pow(2) + (y - 13).pow(2) <= 100 && y <= 14;
            let body = (6..26).contains(&x) && (13..26).contains(&y);
            let hem = (6..26).contains(&x) && (26..29).contains(&y) && ((x - 6) / 4) % 2 == 0;
            let eye = ((10..13).contains(&x) || (19..22).contains(&x)) && (12..17).contains(&y);
            if (head || body || hem) && !eye {
                let i = ((y * 32 + x) * 4) as usize;
                // White elsewhere; macOS uses only alpha as a template mask.
                if !cfg!(target_os = "macos") {
                    rgba[i..i + 3].fill(255);
                }
                rgba[i + 3] = 255;
            }
        }
    }
    if !cfg!(target_os = "macos") {
        // A dark outline keeps the white ghost visible on light panels.
        let original = rgba.clone();
        for y in 1..31usize {
            for x in 1..31usize {
                let i = (y * 32 + x) * 4;
                if original[i + 3] != 0
                    && [i - 4, i + 4, i - 128, i + 128]
                        .iter()
                        .any(|n| original[n + 3] == 0)
                {
                    rgba[i..i + 3].fill(30);
                }
            }
        }
    }
    Icon::from_rgba(rgba, 32, 32).map_err(|e| e.to_string())
}
