//! Shared semantic colors for the native and terminal DiskTree renderers.
use super::Category;
use std::time::SystemTime;

pub type Rgb = [u8; 3];
pub const AGE_LABELS: [&str; 6] = [
    "This week",
    "This month",
    "Six months",
    "This year",
    "Older",
    "Unknown",
];

/// Unknown writes stay separate; future writes are treated as recent clock skew.
pub fn age_bucket(modified: Option<SystemTime>, now: SystemTime) -> usize {
    let Some(modified) = modified else {
        return 5;
    };
    let days = now.duration_since(modified).unwrap_or_default().as_secs() / 86_400;
    match days {
        0..=7 => 0,
        8..=30 => 1,
        31..=182 => 2,
        183..=365 => 3,
        _ => 4,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub card: Rgb,
    pub foreground: Rgb,
    pub muted: Rgb,
    pub primary: Rgb,
    pub warning: Rgb,
    pub danger: Rgb,
    pub charts: [Rgb; 5],
}
impl Palette {
    pub fn category(self, category: Category) -> Rgb {
        match category {
            Category::Code => self.charts[0],
            Category::AgentScratch => self.charts[2],
            Category::Toolchain => self.charts[1],
            Category::Synced => self.charts[4],
            Category::Git => self.danger,
            Category::Media => self.charts[3],
            Category::Documents => self.muted,
            Category::Cache => self.warning,
            Category::Other => mix(self.card, self.muted, 0.65),
        }
    }
    pub fn age(self, bucket: usize) -> Rgb {
        if bucket >= 5 {
            self.muted
        } else {
            mix(self.primary, self.muted, bucket as f32 * 0.2)
        }
    }
    /// Keep label contrast while tinting surfaces and revealing nested levels.
    pub fn fill(self, accent: Rgb, depth: usize) -> Rgb {
        let mut amount = 0.18 + depth.min(4) as f32 * 0.035;
        for _ in 0..8 {
            let fill = mix(self.card, accent, amount);
            if contrast_ratio(self.foreground, fill) >= 4.5 {
                return fill;
            }
            amount *= 0.5;
        }
        self.card
    }
    pub fn marked_fill(self) -> Rgb {
        self.fill(self.danger, 0)
    }
}
pub fn mix(base: Rgb, other: Rgb, amount: f32) -> Rgb {
    let amount = amount.clamp(0., 1.);
    std::array::from_fn(|i| {
        (base[i] as f32 + (other[i] as f32 - base[i] as f32) * amount).round() as u8
    })
}
/// WCAG contrast ratio for two opaque sRGB colors.
pub fn contrast_ratio(left: Rgb, right: Rgb) -> f32 {
    fn luminance(rgb: Rgb) -> f32 {
        let linear = rgb.map(|v| {
            let v = v as f32 / 255.;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        });
        linear[0] * 0.2126 + linear[1] * 0.7152 + linear[2] * 0.0722
    }
    let (a, b) = (luminance(left), luminance(right));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};
    #[test]
    fn age_bands_cover_boundaries_unknown_and_clock_skew() {
        let now = UNIX_EPOCH + Duration::from_secs(1000 * 86400);
        for (days, expected) in [
            (0, 0),
            (7, 0),
            (8, 1),
            (30, 1),
            (31, 2),
            (182, 2),
            (183, 3),
            (365, 3),
            (366, 4),
        ] {
            assert_eq!(
                age_bucket(Some(now - Duration::from_secs(days * 86400)), now),
                expected
            );
        }
        assert_eq!(age_bucket(None, now), 5);
        assert_eq!(age_bucket(Some(now + Duration::from_secs(3600)), now), 0);
    }
    #[test]
    fn tile_contrast_survives_a_bright_accent_on_a_dark_surface() {
        let p = Palette {
            card: [24, 24, 37],
            foreground: [205, 214, 244],
            muted: [171, 178, 203],
            primary: [148, 187, 250],
            warning: [249, 226, 175],
            danger: [249, 199, 213],
            charts: [[255; 3]; 5],
        };
        for category in Category::ALL {
            for depth in 0..8 {
                assert!(contrast_ratio(p.foreground, p.fill(p.category(category), depth)) >= 4.5);
            }
        }
    }
}
