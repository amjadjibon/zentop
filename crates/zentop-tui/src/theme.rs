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
