//! Required upstream UI components; adapter details and license are in UPSTREAM.md.
pub mod palette;
pub mod panels;
pub mod scale;
pub mod treemap_view;
pub mod widgets;

use gpui_kit::base::ThemeAppearance;
use gpui_kit::component::ActiveTheme;
use gpui_kit::{div, App, Div, Hsla, ParentElement, SharedString, Styled};

/// Maps Zentop's semantic tokens onto the small theme surface used upstream.
#[derive(Clone, Debug)]
pub struct Theme {
    pub appearance: ThemeAppearance,
    pub background: Hsla,
    pub foreground: Hsla,
    pub bright: Hsla,
    pub secondary: Hsla,
    pub inset: Hsla,
    pub surface: Hsla,
    pub accent: Hsla,
    pub warning: Hsla,
    pub danger: Hsla,
    pub font: SharedString,
    pub border: Hsla,
    pub colors: zentop_core::disktree::palette::Palette,
}
impl Theme {
    pub fn from(cx: &App) -> Self {
        let t = cx.theme();
        let palette = crate::ui::theme::palette(cx);
        Self {
            appearance: if t.background.l < 0.5 {
                ThemeAppearance::Dark
            } else {
                ThemeAppearance::Light
            },
            background: t.background,
            foreground: t.foreground,
            bright: t.foreground,
            secondary: t.muted_foreground,
            inset: palette.card,
            surface: t.secondary,
            accent: t.primary,
            warning: palette.warning,
            danger: palette.danger,
            font: t.font_family.clone(),
            border: t.border,
            colors: zentop_core::disktree::palette::Palette {
                card: rgb(palette.card),
                foreground: rgb(t.foreground),
                muted: rgb(t.muted_foreground),
                primary: rgb(t.primary),
                warning: rgb(palette.warning),
                danger: rgb(palette.danger),
                charts: palette.charts.map(rgb),
            },
        }
    }
    pub fn hover_fill(&self) -> Hsla {
        self.accent.opacity(0.1)
    }
    pub fn divider(&self) -> Hsla {
        self.border
    }
}
pub trait ActiveDiskTheme {
    fn disk_theme(&self) -> Theme;
}
impl ActiveDiskTheme for App {
    fn disk_theme(&self) -> Theme {
        Theme::from(self)
    }
}
fn keycap(keys: impl Into<SharedString>, cx: &App) -> Div {
    let theme = Theme::from(cx);
    div()
        .px(scale::space::XS)
        .py(scale::space::XXS)
        .border_1()
        .border_color(theme.border)
        .text_size(scale::text::CAPTION)
        .text_color(theme.bright)
        .child(keys.into())
}

pub fn rgb(color: Hsla) -> [u8; 3] {
    let c = color.to_rgb();
    [c.r, c.g, c.b].map(|v| (v.clamp(0., 1.) * 255.).round() as u8)
}
