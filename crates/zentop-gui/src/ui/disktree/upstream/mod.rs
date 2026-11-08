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
        }
    }
    pub fn hover_fill(&self) -> Hsla {
        self.accent.opacity(0.1)
    }
    pub fn divider(&self) -> Hsla {
        self.border
    }
    #[cfg(test)]
    fn fixture(appearance: ThemeAppearance) -> Self {
        let dark = matches!(appearance, ThemeAppearance::Dark);
        let c = |value| gpui_kit::rgb(value).into();
        Self {
            appearance,
            background: c(if dark { 0x1e1e2e } else { 0xfafafa }),
            foreground: c(if dark { 0xcdd6f4 } else { 0x202030 }),
            bright: c(if dark { 0xcdd6f4 } else { 0x202030 }),
            secondary: c(0x8890a0),
            inset: c(if dark { 0x181825 } else { 0xf3f3f3 }),
            surface: c(if dark { 0x232333 } else { 0xeaeaea }),
            accent: c(0x89b4fa),
            warning: c(0xf9c457),
            danger: c(0xf38ba8),
            font: "monospace".into(),
            border: c(0x505160),
        }
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
