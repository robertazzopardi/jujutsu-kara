//! VS Code JSON color theme → syntect [`Theme`].
//!
//! Enough for kara: `editor.foreground` / `editor.background` and `tokenColors`.
//! Expects strict JSON (no comments) and `#rgb` / `#rrggbb` / `#rrggbbaa` colors.

use std::collections::HashMap;
use std::str::FromStr;

use serde::Deserialize;
use syntect::highlighting::{
    Color, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings,
};

#[derive(Debug, Deserialize)]
struct VsCodeTheme {
    name: Option<String>,
    #[serde(default)]
    colors: HashMap<String, Option<String>>,
    #[serde(rename = "tokenColors", default)]
    token_colors: Vec<TokenColor>,
}

#[derive(Debug, Deserialize)]
struct TokenColor {
    scope: Option<ScopeField>,
    #[serde(default)]
    settings: TokenSettings,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ScopeField {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Default, Deserialize)]
struct TokenSettings {
    foreground: Option<String>,
    background: Option<String>,
}

/// Parse a VS Code theme JSON string into a syntect theme.
pub fn theme_from_json(json: &str) -> Result<Theme, String> {
    let vs: VsCodeTheme = serde_json::from_str(json).map_err(|e| e.to_string())?;

    let mut settings = ThemeSettings::default();
    if let Some(bg) = vs
        .colors
        .get("editor.background")
        .and_then(|c| c.as_deref())
    {
        settings.background = parse_color(bg)?;
    }
    if let Some(fg) = vs
        .colors
        .get("editor.foreground")
        .or_else(|| vs.colors.get("foreground"))
        .and_then(|c| c.as_deref())
    {
        settings.foreground = parse_color(fg)?;
        settings.caret = settings.foreground;
    }

    let mut scopes = Vec::with_capacity(vs.token_colors.len());
    for token in vs.token_colors {
        let scope = match token.scope {
            Some(ScopeField::One(s)) => parse_scopes(&s)?,
            Some(ScopeField::Many(parts)) => parse_scopes(&parts.join(","))?,
            None => parse_scopes("*")?,
        };
        scopes.push(ThemeItem {
            scope,
            style: StyleModifier {
                foreground: match token.settings.foreground.as_deref() {
                    Some(c) => parse_color(c)?,
                    None => None,
                },
                background: match token.settings.background.as_deref() {
                    Some(c) => parse_color(c)?,
                    None => None,
                },
                // Terminal escape path only uses colors; skip fontStyle.
                font_style: None,
            },
        });
    }

    Ok(Theme {
        name: vs.name,
        author: None,
        settings,
        scopes,
    })
}

fn parse_scopes(s: &str) -> Result<ScopeSelectors, String> {
    ScopeSelectors::from_str(s).map_err(|e| format!("scope '{s}': {e:?}"))
}

fn parse_color(s: &str) -> Result<Option<Color>, String> {
    Color::from_str(s)
        .map(Some)
        .map_err(|_| format!("invalid color '{s}'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_dark_default_loads() {
        let theme = theme_from_json(include_str!("../assets/github-dark-default.json")).unwrap();
        assert_eq!(theme.name.as_deref(), Some("GitHub Dark Default"));
        assert!(!theme.scopes.is_empty());
        assert!(theme.settings.foreground.is_some());
        assert!(theme.settings.background.is_some());
    }
}
