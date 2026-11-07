//! NeoHtop palettes applied to the gpui-component theme.
//!
//! `assets/themes.json` holds semantic roles already resolved for contrast by
//! `site/lib/utils/theme-tokens.js` (regenerate with `node scripts/export-themes.mjs`).
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{px, App, Global, Hsla, Rgba};
use serde::Deserialize;
use std::sync::LazyLock;

pub const DEFAULT_THEME: &str = "catppuccin";

#[derive(Deserialize)]
struct Tokens {
    name: String,
    label: String,
    dark: bool,
    radius: f32,
    background: String,
    foreground: String,
    card: String,
    popover: String,
    popover_foreground: String,
    primary: String,
    primary_foreground: String,
    secondary: String,
    secondary_foreground: String,
    muted: String,
    muted_foreground: String,
    accent: String,
    accent_foreground: String,
    destructive: String,
    destructive_foreground: String,
    border: String,
    input: String,
    ring: String,
    chart_1: String,
    chart_2: String,
    chart_3: String,
    chart_4: String,
    chart_5: String,
    status_running: String,
    status_stopped: String,
    status_idle: String,
    scrollbar_thumb: String,
    scrollbar_thumb_hover: String,
    yellow: String,
}

static THEMES: LazyLock<Vec<Tokens>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../assets/themes.json")).expect("valid themes.json")
});

/// Theme names and labels in display order.
pub fn all() -> impl Iterator<Item = (&'static str, &'static str)> {
    THEMES.iter().map(|t| (t.name.as_str(), t.label.as_str()))
}

pub fn exists(name: &str) -> bool {
    THEMES.iter().any(|t| t.name == name)
}

/// Colors NeoHtop paints itself, beyond the component theme.
#[derive(Clone, Copy)]
pub struct Palette {
    pub card: Hsla,
    pub charts: [Hsla; 5],
    pub running: Hsla,
    pub stopped: Hsla,
    pub idle: Hsla,
    pub warning: Hsla,
    pub danger: Hsla,
}
impl Global for Palette {}

pub fn palette(cx: &App) -> Palette {
    *cx.global::<Palette>()
}

fn hex(value: &str) -> Hsla {
    let digits = value.trim_start_matches('#');
    let parsed = u32::from_str_radix(digits, 16).unwrap_or(0);
    let (rgb, alpha) = if digits.len() == 8 {
        (parsed >> 8, (parsed & 0xff) as f32 / 255.0)
    } else {
        (parsed, 1.0)
    };
    let mut color: Hsla = Rgba {
        r: ((rgb >> 16) & 0xff) as f32 / 255.0,
        g: ((rgb >> 8) & 0xff) as f32 / 255.0,
        b: (rgb & 0xff) as f32 / 255.0,
        a: 1.0,
    }
    .into();
    color.a = alpha;
    color
}

/// Mixes `amount` of `other` into `base`, for hover and pressed states.
fn mix(base: Hsla, other: Hsla, amount: f32) -> Hsla {
    let (a, b): (Rgba, Rgba) = (base.into(), other.into());
    Rgba {
        r: a.r + (b.r - a.r) * amount,
        g: a.g + (b.g - a.g) * amount,
        b: a.b + (b.b - a.b) * amount,
        a: a.a,
    }
    .into()
}

/// Applies a palette by name, falling back to the default for unknown names.
pub fn apply(name: &str, cx: &mut App) {
    let tokens = THEMES
        .iter()
        .find(|t| t.name == name)
        .or_else(|| THEMES.iter().find(|t| t.name == DEFAULT_THEME))
        .expect("default theme exists");
    let mode = if tokens.dark {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    };
    Theme::change(mode, None, cx);

    let background = hex(&tokens.background);
    let foreground = hex(&tokens.foreground);
    let card = hex(&tokens.card);
    let primary = hex(&tokens.primary);
    let secondary = hex(&tokens.secondary);
    let muted = hex(&tokens.muted);
    let accent = hex(&tokens.accent);
    let danger = hex(&tokens.destructive);
    let border = hex(&tokens.border);
    let toward = if tokens.dark { foreground } else { background };
    let hover = |color: Hsla| mix(color, toward, 0.1);
    let pressed = |color: Hsla| mix(color, toward, 0.18);
    let charts = [
        hex(&tokens.chart_1),
        hex(&tokens.chart_2),
        hex(&tokens.chart_3),
        hex(&tokens.chart_4),
        hex(&tokens.chart_5),
    ];

    Theme::update(cx, |theme| {
        theme.radius = px(tokens.radius);
        theme.radius_lg = px(tokens.radius + 2.0);
        let c = &mut theme.colors;
        c.background = background;
        c.foreground = foreground;
        c.border = border;
        c.input = hex(&tokens.input);
        c.ring = hex(&tokens.ring);
        c.caret = foreground;
        c.selection = primary.opacity(0.3);
        c.popover = hex(&tokens.popover);
        c.popover_foreground = hex(&tokens.popover_foreground);
        c.primary = primary;
        c.primary_hover = hover(primary);
        c.primary_active = pressed(primary);
        c.primary_foreground = hex(&tokens.primary_foreground);
        c.secondary = secondary;
        c.secondary_hover = hover(secondary);
        c.secondary_active = pressed(secondary);
        c.secondary_foreground = hex(&tokens.secondary_foreground);
        c.muted = muted;
        c.muted_foreground = hex(&tokens.muted_foreground);
        c.accent = accent;
        c.accent_foreground = hex(&tokens.accent_foreground);
        c.danger = danger;
        c.danger_hover = hover(danger);
        c.danger_active = pressed(danger);
        c.danger_foreground = hex(&tokens.destructive_foreground);
        c.link = primary;
        c.link_hover = hover(primary);
        c.link_active = pressed(primary);
        c.chart_1 = charts[0];
        c.chart_2 = charts[1];
        c.chart_3 = charts[2];
        c.chart_4 = charts[3];
        c.chart_5 = charts[4];
        c.chart_grid = border.opacity(0.5);
        c.progress_bar = primary;
        c.title_bar = card;
        c.title_bar_border = border;
        c.group_box = card;
        c.group_box_foreground = foreground;
        c.list = background;
        c.list_even = background;
        c.list_head = card;
        c.list_hover = accent;
        c.list_active = primary.opacity(0.12);
        c.list_active_border = primary.opacity(0.5);
        c.table = background;
        c.table_even = background;
        c.table_head = card;
        c.table_head_foreground = c.muted_foreground;
        c.table_hover = accent;
        c.table_active = primary.opacity(0.12);
        c.table_active_border = primary.opacity(0.5);
        c.table_row_border = border.opacity(0.5);
        c.scrollbar = background.opacity(0.0);
        c.scrollbar_thumb = hex(&tokens.scrollbar_thumb);
        c.scrollbar_thumb_hover = hex(&tokens.scrollbar_thumb_hover);
        c.switch = secondary;
        c.slider_bar = primary;
        c.slider_thumb = foreground;
        c.tab_bar = card;
        c.tab_bar_segmented = muted;
        c.tab = card;
        c.tab_active = background;
        c.tab_foreground = c.muted_foreground;
        c.tab_active_foreground = foreground;
        c.sidebar = card;
        c.sidebar_border = border;
        c.sidebar_foreground = foreground;
        c.sidebar_accent = accent;
        c.sidebar_accent_foreground = foreground;
        c.sidebar_primary = primary;
        c.sidebar_primary_foreground = c.primary_foreground;
        c.skeleton = muted;
        c.window_border = border;
        c.overlay = Hsla::black().opacity(if tokens.dark { 0.5 } else { 0.25 });
    });
    cx.set_global(Palette {
        card,
        charts,
        running: hex(&tokens.status_running),
        stopped: hex(&tokens.status_stopped),
        idle: hex(&tokens.status_idle),
        warning: hex(&tokens.yellow),
        danger,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_theme_parses_and_default_exists() {
        assert_eq!(THEMES.len(), 24);
        assert!(exists(DEFAULT_THEME));
        assert!(THEMES.iter().any(|t| !t.dark));
        let white = hex("#ffffff");
        assert!((white.l - 1.0).abs() < 1e-3);
        assert!((hex("#00000080").a - 128.0 / 255.0).abs() < 1e-3);
    }
}
