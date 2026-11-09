// Copyright (c) 2026 Tobi Lütke
// SPDX-License-Identifier: MIT
// Adapted from tobi/disktree 9b18b272ca3c5a145f6ff58eaf89dc4d311eeaea, crates/disktree-app/src/palette.rs.
// Full license: licenses/disktree-MIT.md. See UPSTREAM.md for adapters.

//! Zentop semantic palette adapters for the copied DiskTree painter.
use super::Theme;
use gpui_kit::base::ThemeAppearance;
use gpui_kit::{Hsla, Rgba};
use zentop_core::disktree::{palette::AGE_LABELS, Category};

pub const AGE_BUCKETS: [(usize, &str); 6] = [
    (0, AGE_LABELS[0]),
    (1, AGE_LABELS[1]),
    (2, AGE_LABELS[2]),
    (3, AGE_LABELS[3]),
    (4, AGE_LABELS[4]),
    (5, AGE_LABELS[5]),
];
fn color(rgb: [u8; 3]) -> Hsla {
    gpui_kit::rgb((u32::from(rgb[0]) << 16) | (u32::from(rgb[1]) << 8) | u32::from(rgb[2])).into()
}
const fn dark(theme: &Theme) -> bool {
    matches!(theme.appearance, ThemeAppearance::Dark)
}
pub fn category_fill(theme: &Theme, category: Category, depth: u32) -> Hsla {
    color(
        theme
            .colors
            .fill(theme.colors.category(category), depth as usize),
    )
}
pub fn category_accent(theme: &Theme, category: Category) -> Hsla {
    color(theme.colors.category(category))
}
pub fn age_fill(theme: &Theme, bucket: usize, depth: u32) -> Hsla {
    color(theme.colors.fill(theme.colors.age(bucket), depth as usize))
}
pub fn age_accent(theme: &Theme, bucket: usize) -> Hsla {
    color(theme.colors.age(bucket))
}
pub const fn highlight(theme: &Theme) -> Hsla {
    theme.accent
}

/// The diagonal hatch over reclaimable space: quiet enough to leave the hue
/// readable, visible on every fill.
pub fn hatch(theme: &Theme) -> Hsla {
    if dark(theme) {
        theme.bright.opacity(0.16)
    } else {
        theme.foreground.opacity(0.18)
    }
}

/// Linear interpolation between two colours, in RGB: interpolating hue
/// would drag a colour around the wheel on its way to a grey.
pub fn mix(from: Hsla, to: Hsla, t: f32) -> Hsla {
    let t = t.clamp(0.0, 1.0);
    let lerp = |a: f32, b: f32| (b - a).mul_add(t, a);
    let (a, b) = (from.to_rgb(), to.to_rgb());
    Hsla::from(Rgba {
        r: lerp(a.r, b.r),
        g: lerp(a.g, b.g),
        b: lerp(a.b, b.b),
        a: lerp(a.a, b.a),
    })
}

/// A tile's name on top of its fill.
pub fn label_color(theme: &Theme, _depth: u32) -> Hsla {
    theme.foreground
}
