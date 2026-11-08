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
        .with_icon(mark()?)
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

/// The Zentop mark: an open ensō ring with a dot in its gap, anti-aliased at
/// 32×32. macOS treats it as a template (only alpha counts), so the system
/// picks menu-bar contrast in light and dark mode; elsewhere it uses the brand
/// colors.
fn mark() -> Result<Icon, String> {
    Icon::from_rgba(mark_pixels(), MARK_SIZE as u32, MARK_SIZE as u32).map_err(|e| e.to_string())
}

const MARK_SIZE: usize = 32;

fn mark_pixels() -> Vec<u8> {
    const CENTER: f32 = 16.0;
    const RADIUS: f32 = 10.5;
    const HALF_STROKE: f32 = 1.8;
    const DOT: f32 = 2.7;
    // Screen angles (y down): the ring runs clockwise from START to END,
    // leaving a gap at the top right where the dot sits. The gap is wider than
    // the app icon's so ring and dot stay separate at menu-bar size.
    const START: f32 = -20.0;
    const END: f32 = 250.0;
    let at = |degrees: f32| {
        let t = degrees.to_radians();
        (CENTER + RADIUS * t.cos(), CENTER + RADIUS * t.sin())
    };
    let ends = [at(START), at(END)];
    let dot = at((START + 360.0 + END) / 2.0);
    let coverage = |distance: f32, half: f32| (half + 0.5 - distance).clamp(0.0, 1.0);

    let mut rgba = vec![0u8; MARK_SIZE * MARK_SIZE * 4];
    for y in 0..MARK_SIZE {
        for x in 0..MARK_SIZE {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let (dx, dy) = (px - CENTER, py - CENTER);
            let angle = dy.atan2(dx).to_degrees().rem_euclid(360.0);
            let on_arc = angle >= START.rem_euclid(360.0) || angle <= END;
            // Distance to the arc's centerline, with round caps at both ends.
            let to_ring = if on_arc {
                ((dx * dx + dy * dy).sqrt() - RADIUS).abs()
            } else {
                ends.iter()
                    .map(|(ex, ey)| ((px - ex).powi(2) + (py - ey).powi(2)).sqrt())
                    .fold(f32::MAX, f32::min)
            };
            let ring = coverage(to_ring, HALF_STROKE);
            let spot = coverage(((px - dot.0).powi(2) + (py - dot.1).powi(2)).sqrt(), DOT);
            let alpha = ring.max(spot);
            if alpha == 0.0 {
                continue;
            }
            let i = (y * MARK_SIZE + x) * 4;
            if !cfg!(target_os = "macos") {
                let color: [u8; 3] = if spot > ring {
                    [0xff, 0xc3, 0x7a]
                } else {
                    [0x7e, 0xe0, 0xc6]
                };
                rgba[i..i + 3].copy_from_slice(&color);
            }
            rgba[i + 3] = (alpha * 255.0).round() as u8;
        }
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_has_a_ring_with_a_gap_and_a_dot() {
        let px = mark_pixels();
        let alpha = |x: usize, y: usize| px[(y * MARK_SIZE + x) * 4 + 3];
        // Solid ring on the left and bottom, hollow center.
        assert!(alpha(5, 16) > 200);
        assert!(alpha(16, 26) > 200);
        assert_eq!(alpha(16, 16), 0);
        // The dot sits in the gap at the top right; the ring itself is open there.
        let gap = (295f32).to_radians();
        let (gx, gy) = (16.0 + 10.5 * gap.cos(), 16.0 + 10.5 * gap.sin());
        assert!(alpha(gx as usize, gy as usize) > 200);
        let beside = (268f32).to_radians();
        assert_eq!(
            alpha(
                (16.0 + 10.5 * beside.cos()) as usize,
                (16.0 + 10.5 * beside.sin()) as usize
            ),
            0
        );
    }
}
