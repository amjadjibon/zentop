//! Use the desktop's semantic color tokens, converted to terminal true color.
use ratatui::style::Color;
use serde::Deserialize;
use std::{io, str::FromStr};

#[derive(Clone, Debug, Deserialize)]
pub struct Theme {
    pub name: String,
    pub label: String,
    #[serde(deserialize_with = "color")]
    pub background: Color,
    #[serde(deserialize_with = "color")]
    pub foreground: Color,
    #[serde(deserialize_with = "color")]
    pub card: Color,
    #[serde(deserialize_with = "color")]
    pub primary: Color,
    #[serde(deserialize_with = "color")]
    pub muted_foreground: Color,
    #[serde(deserialize_with = "color")]
    pub accent: Color,
    #[serde(deserialize_with = "color")]
    pub border: Color,
    #[serde(deserialize_with = "color")]
    pub chart_1: Color,
    #[serde(deserialize_with = "color")]
    pub chart_2: Color,
    #[serde(deserialize_with = "color")]
    pub chart_3: Color,
    #[serde(deserialize_with = "color")]
    pub chart_4: Color,
    #[serde(deserialize_with = "color")]
    pub chart_5: Color,
    #[serde(deserialize_with = "color")]
    pub status_running: Color,
    #[serde(deserialize_with = "color")]
    pub yellow: Color,
    #[serde(deserialize_with = "color")]
    pub destructive: Color,
}

fn color<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Color, D::Error> {
    let value = String::deserialize(deserializer)?;
    Color::from_str(&value).map_err(serde::de::Error::custom)
}

pub fn load() -> io::Result<Vec<Theme>> {
    let themes: Vec<Theme> = serde_json::from_str(include_str!("../../../assets/themes.json"))?;
    if themes.is_empty() {
        return Err(io::Error::other("No themes available"));
    }
    Ok(themes)
}

impl Theme {
    pub fn disktree_palette(&self) -> zentop_core::disktree::palette::Palette {
        let rgb = |color| match color {
            Color::Rgb(r, g, b) => [r, g, b],
            _ => [0, 0, 0],
        };
        zentop_core::disktree::palette::Palette {
            card: rgb(self.card),
            foreground: rgb(self.foreground),
            muted: rgb(self.muted_foreground),
            primary: rgb(self.primary),
            warning: rgb(self.yellow),
            danger: rgb(self.destructive),
            charts: [
                self.chart_1,
                self.chart_2,
                self.chart_3,
                self.chart_4,
                self.chart_5,
            ]
            .map(rgb),
        }
    }
}
