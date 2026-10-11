//! Theme resolution (embedded GitHub Dark Default + two-face names).

use syntect::highlighting::Theme;
use two_face::theme::{LazyThemeSet, extra};

use crate::vscode_theme;

/// Dark green / red washes (IDE-style), truecolor.
pub const ADD_BG: (u8, u8, u8) = (18, 42, 28);
pub const DEL_BG: (u8, u8, u8) = (52, 24, 28);

pub fn sgr_bg((r, g, b): (u8, u8, u8)) -> String {
    format!("\x1b[48;2;{r};{g};{b}m")
}

fn theme_set() -> LazyThemeSet {
    LazyThemeSet::from(extra())
}

/// Primer GitHub Dark Default — always dark fg/bg token colors (truecolor), independent of terminal theme.
fn github_dark_default() -> Theme {
    vscode_theme::theme_from_json(include_str!("../assets/github-dark-default.json"))
        .expect("embedded GitHub Dark Default JSON")
}

fn is_github_dark_alias(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "github" | "github dark" | "github dark default" | "github-dark" | "github-dark-default"
    )
}

pub fn resolve_theme(name: &str) -> Theme {
    if is_github_dark_alias(name) {
        return github_dark_default();
    }
    let themes = theme_set();
    themes.get(name).cloned().unwrap_or_else(|| {
        eprintln!("unknown theme '{name}', falling back to GitHub Dark Default");
        github_dark_default()
    })
}
