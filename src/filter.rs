//! Line-oriented filter: highlight code payloads in git-unified / jj color-words diffs.

use std::io::{self, BufRead, Write};

use syntect::easy::HighlightLines;
use syntect::highlighting::Theme;
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::as_24_bit_terminal_escaped;

use crate::ansi::{
    DiffTone, color_words_display_payload, overlay_word_diff, split_ansi_at_stripped, strip_ansi,
    underline_ranges,
};
use crate::theme::{ADD_BG, DEL_BG, sgr_bg};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Auto,
    Git,
    ColorWords,
}

pub fn process(
    mut input: impl BufRead,
    out: &mut impl Write,
    ps: &SyntaxSet,
    theme: &Theme,
    word_underline: bool,
) -> io::Result<()> {
    let mut mode = Mode::Auto;
    let mut syntax: Option<&SyntaxReference> = None;
    // ponytail: per-line HighlightLines loses multi-line state; dual old/new stacks if that bites
    let mut buf = String::new();

    loop {
        buf.clear();
        let n = input.read_line(&mut buf)?;
        if n == 0 {
            break;
        }
        let had_nl = buf.ends_with('\n');
        let line = if had_nl {
            &buf[..buf.len() - 1]
        } else {
            buf.as_str()
        };
        let line = line.strip_suffix('\r').unwrap_or(line);
        let stripped = strip_ansi(line);

        if stripped.starts_with("diff ") {
            mode = Mode::Git;
            syntax = path_from_diff_header(&stripped).and_then(|p| syntax_for_path(ps, &p));
            write_raw(out, line, had_nl)?;
            continue;
        }

        if let Some(path) = path_from_color_words_header(&stripped) {
            mode = Mode::ColorWords;
            syntax = syntax_for_path(ps, path);
            write_raw(out, line, had_nl)?;
            continue;
        }

        match mode {
            Mode::ColorWords => {
                if let Some(payload_at) = color_words_payload_at(&stripped) {
                    let (prefix, _) = split_ansi_at_stripped(line, payload_at);
                    let bg = color_words_line_bg(&stripped);
                    // Keep jj word-diff marks: underline + red/green (not harsh syntax colors).
                    let fallback = match bg {
                        Some(bg) if bg == ADD_BG => DiffTone::Add,
                        Some(_) => DiffTone::Remove,
                        None => DiffTone::Add,
                    };
                    // Delete-only lines are all "removed"; dual-sided lines inline old+new —
                    // drop removed tokens so syntect sees valid right-side text.
                    let keep_removes = bg == Some(DEL_BG);
                    let (payload, under) =
                        color_words_display_payload(line, payload_at, keep_removes, fallback);
                    let mut highlighted = syntax
                        .map(|syn| highlight_payload(ps, theme, syn, &payload))
                        .unwrap_or(payload);
                    if word_underline && !under.is_empty() {
                        highlighted = overlay_word_diff(&highlighted, &under);
                    }
                    write!(out, "{prefix}")?;
                    if let Some(bg) = bg {
                        write!(out, "{}", sgr_bg(bg))?;
                    }
                    write!(out, "{highlighted}\x1b[0m")?;
                    if had_nl {
                        out.write_all(b"\n")?;
                    }
                } else {
                    write_raw(out, line, had_nl)?;
                }
            }
            Mode::Git => {
                if stripped.starts_with("+++ ") {
                    if let Some(p) = path_from_plus_header(&stripped) {
                        syntax = syntax_for_path(ps, &p);
                    }
                    write_raw(out, line, had_nl)?;
                } else if is_passthrough_git_line(&stripped) {
                    write_raw(out, line, had_nl)?;
                } else if let Some(marker) = content_marker(&stripped) {
                    let payload_at = marker.len_utf8();
                    let payload = &stripped[payload_at..];
                    let mut highlighted = syntax.map_or_else(
                        || payload.to_string(),
                        |syn| highlight_payload(ps, theme, syn, payload),
                    );
                    if word_underline {
                        let fallback = match marker {
                            '-' => DiffTone::Remove,
                            _ => DiffTone::Add,
                        };
                        let under = underline_ranges(line, payload_at, fallback);
                        if !under.is_empty() {
                            highlighted = overlay_word_diff(&highlighted, &under);
                        }
                    }
                    write_content(out, marker, &highlighted, had_nl)?;
                } else {
                    write_raw(out, line, had_nl)?;
                }
            }
            Mode::Auto => write_raw(out, line, had_nl)?,
        }
    }
    out.flush()
}

fn write_raw(out: &mut impl Write, line: &str, had_nl: bool) -> io::Result<()> {
    out.write_all(line.as_bytes())?;
    if had_nl {
        out.write_all(b"\n")?;
    }
    Ok(())
}

fn write_content(
    out: &mut impl Write,
    marker: char,
    highlighted: &str,
    had_nl: bool,
) -> io::Result<()> {
    // IDE-style: full-line wash + marker, syntax colors on the payload.
    match marker {
        '+' => write!(
            out,
            "{}\x1b[38;2;144;190;109m+\x1b[39m{highlighted}\x1b[0m",
            sgr_bg(ADD_BG)
        )?,
        '-' => write!(
            out,
            "{}\x1b[38;2;224;108;117m-\x1b[39m{highlighted}\x1b[0m",
            sgr_bg(DEL_BG)
        )?,
        ' ' => write!(out, " {highlighted}\x1b[0m")?,
        _ => unreachable!(),
    }
    if had_nl {
        out.write_all(b"\n")?;
    }
    Ok(())
}

fn highlight_payload(
    ps: &SyntaxSet,
    theme: &Theme,
    syntax: &SyntaxReference,
    payload: &str,
) -> String {
    let mut h = HighlightLines::new(syntax, theme);
    // HighlightLines expects a trailing newline for some grammars
    let owned;
    let line = if payload.ends_with('\n') {
        payload
    } else {
        owned = format!("{payload}\n");
        owned.as_str()
    };
    h.highlight_line(line, ps).map_or_else(
        |_| payload.to_string(),
        |ranges| {
            let mut s = as_24_bit_terminal_escaped(&ranges[..], false);
            // drop syntect's trailing reset so callers can wrap with diff backgrounds
            if let Some(trimmed) = s.strip_suffix("\x1b[0m") {
                s = trimmed.to_string();
            }
            if !payload.ends_with('\n') && s.ends_with('\n') {
                s.pop();
            }
            if s.ends_with('\n') {
                s.pop();
            }
            s
        },
    )
}

/// Only-left → deleted wash; only-right → added wash; both → context (no wash).
fn color_words_line_bg(stripped: &str) -> Option<(u8, u8, u8)> {
    if stripped.len() < 9 {
        return None;
    }
    let left_blank = stripped[..4].bytes().all(|c| c == b' ');
    let right_blank = stripped[5..9].bytes().all(|c| c == b' ');
    match (left_blank, right_blank) {
        (true, false) => Some(ADD_BG),
        (false, true) => Some(DEL_BG),
        _ => None,
    }
}

fn content_marker(stripped: &str) -> Option<char> {
    let mut chars = stripped.chars();
    let c = chars.next()?;
    matches!(c, ' ' | '+' | '-').then_some(c).filter(|_| {
        // avoid treating "+++ b/file" / "--- a/file" as content (already handled)
        // and "diff " etc.
        true
    })
}

fn is_passthrough_git_line(stripped: &str) -> bool {
    stripped.starts_with("diff ")
        || stripped.starts_with("index ")
        || stripped.starts_with("new file mode ")
        || stripped.starts_with("deleted file mode ")
        || stripped.starts_with("old mode ")
        || stripped.starts_with("new mode ")
        || stripped.starts_with("similarity index ")
        || stripped.starts_with("rename from ")
        || stripped.starts_with("rename to ")
        || stripped.starts_with("copy from ")
        || stripped.starts_with("copy to ")
        || stripped.starts_with("Binary files ")
        || stripped.starts_with("--- ")
        || stripped.starts_with("+++ ")
        || stripped.starts_with("@@")
        || stripped.starts_with("\\ No newline")
}

/// `Modified regular file path:` / `Added … file path:` / renames `old => new:`
fn path_from_color_words_header(stripped: &str) -> Option<&str> {
    let s = stripped.strip_suffix(':')?;
    let path = if let Some(i) = s.find(" file ") {
        &s[i + " file ".len()..]
    } else if let Some(rest) = s.strip_prefix("Renamed ") {
        rest
    } else { s.strip_prefix("Copied ")? };
    if path.is_empty() {
        return None;
    }
    // "old.rs => new.rs"
    Some(match path.rsplit_once(" => ") {
        Some((_, new)) => new,
        None => path,
    })
}

/// Fixed-width jj color-words columns: `LLLL RRRR:` then code payload.
fn color_words_payload_at(stripped: &str) -> Option<usize> {
    let b = stripped.as_bytes();
    if b.len() < 10 || b[4] != b' ' || b[9] != b':' {
        return None;
    }
    let left = &stripped[..4];
    let right = &stripped[5..9];
    let ok = |col: &str| col.chars().all(|c| c.is_ascii_digit() || c == ' ');
    if !ok(left) || !ok(right) {
        return None;
    }
    if left.bytes().all(|c| c == b' ') && right.bytes().all(|c| c == b' ') {
        return None;
    }
    Some(10)
}

fn path_from_diff_header(stripped: &str) -> Option<String> {
    // diff --git a/path b/path
    if let Some(rest) = stripped.strip_prefix("diff --git ") {
        let mut parts = rest.split_whitespace();
        let a = parts.next()?;
        let b = parts.next()?;
        let path = normalize_diff_path(b);
        if path == "/dev/null" {
            return Some(normalize_diff_path(a).to_string());
        }
        return Some(path.to_string());
    }
    // diff -ruN left/path right/path
    let path = stripped.split_whitespace().last()?;
    Some(normalize_diff_path(path).to_string())
}

fn path_from_plus_header(stripped: &str) -> Option<String> {
    // +++ b/path or +++ right/path or +++ /dev/null
    let rest = stripped.strip_prefix("+++ ")?;
    let path = rest.split('\t').next().unwrap_or(rest);
    if path == "/dev/null" {
        return None;
    }
    Some(normalize_diff_path(path).to_string())
}

fn normalize_diff_path(path: &str) -> &str {
    path.strip_prefix("b/")
        .or_else(|| path.strip_prefix("a/"))
        .or_else(|| path.strip_prefix("right/"))
        .or_else(|| path.strip_prefix("left/"))
        .unwrap_or(path)
}

fn syntax_for_path<'a>(ps: &'a SyntaxSet, path: &str) -> Option<&'a SyntaxReference> {
    let name = path.rsplit('/').next().unwrap_or(path);
    // Cargo.lock is TOML; syntect/two-face don't register the .lock extension.
    let ext = if name == "Cargo.lock" {
        "toml"
    } else {
        name.rsplit_once('.').map_or("", |(_, e)| e)
    };
    if let Some(syn) = ps.find_syntax_by_extension(ext) {
        return Some(syn);
    }
    ps.find_syntax_by_name(name)
        .or_else(|| ps.find_syntax_by_extension(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::resolve_theme;

    fn run(input: &str) -> String {
        run_opts(input, false)
    }

    fn run_opts(input: &str, word_underline: bool) -> String {
        let ps = two_face::syntax::extra_newlines();
        let theme = resolve_theme("GitHub Dark Default");
        let mut out = Vec::new();
        process(input.as_bytes(), &mut out, &ps, &theme, word_underline).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn headers_and_hunks_unchanged() {
        let input = "\
\x1b[1mdiff --git a/main.rs b/main.rs\x1b[0m
\x1b[1mindex 111..222 100644\x1b[0m
\x1b[1m--- a/main.rs\x1b[0m
\x1b[1m+++ b/main.rs\x1b[0m
\x1b[38;5;6m@@ -1,2 +1,2 @@\x1b[39m
 fn main() {
-    println!(\"a\");
+    println!(\"b\");
 }
";
        let out = run(input);
        for header in [
            "\x1b[1mdiff --git a/main.rs b/main.rs\x1b[0m",
            "\x1b[1mindex 111..222 100644\x1b[0m",
            "\x1b[1m--- a/main.rs\x1b[0m",
            "\x1b[1m+++ b/main.rs\x1b[0m",
            "\x1b[38;5;6m@@ -1,2 +1,2 @@\x1b[39m",
        ] {
            assert!(
                out.contains(header),
                "missing passthrough {header:?}\n{out}"
            );
        }
    }

    #[test]
    fn code_lines_get_syntax_ansi() {
        let input = "\
diff --git a/main.rs b/main.rs
--- a/main.rs
+++ b/main.rs
@@ -1 +1 @@
-fn old() {}
+fn new() {}
";
        let out = run(input);
        assert!(
            out.contains("\x1b[38;2;"),
            "expected syntect truecolor in:\n{out}"
        );
        assert!(
            out.contains(&sgr_bg(ADD_BG)),
            "expected add background:\n{out}"
        );
        assert!(
            out.contains(&sgr_bg(DEL_BG)),
            "expected del background:\n{out}"
        );
    }

    #[test]
    fn diff_run_headers_highlighted() {
        let input = "\
diff -ruN left/main.rs right/main.rs
--- left/main.rs	2026-01-01
+++ right/main.rs	2026-01-01
@@ -1 +1 @@
-fn old() {}
+fn new() {}
";
        let out = run(input);
        assert!(out.contains("diff -ruN left/main.rs right/main.rs"));
        assert!(
            out.contains("\x1b[38;2;"),
            "expected syntect truecolor in:\n{out}"
        );
    }

    #[test]
    fn color_words_highlights_code_keeps_header() {
        let input = "\
\x1b[38;5;3mModified regular file main.rs:\x1b[39m
   1    1: fn main() {
   2     :     println!(\"hello\");
        2:     println!(\"hello world\");
";
        let out = run(input);
        assert!(
            out.contains("\x1b[38;5;3mModified regular file main.rs:\x1b[39m"),
            "header must pass through:\n{out}"
        );
        assert!(
            out.contains("\x1b[38;2;"),
            "expected syntect on code:\n{out}"
        );
        assert!(
            out.contains("   1    1:"),
            "line-number prefix kept:\n{out}"
        );
    }

    #[test]
    fn color_words_payload_at_columns() {
        assert_eq!(color_words_payload_at("   1    1: fn main() {"), Some(10));
        assert_eq!(color_words_payload_at("        2:     println!"), Some(10));
        assert_eq!(color_words_payload_at("Modified regular file x:"), None);
    }

    #[test]
    fn preserves_jj_token_underlines() {
        // payload starts at col 10; " world" is underlined + green by jj
        let input = "\
Modified regular file main.rs:
\x1b[38;5;1m   2\x1b[39m \x1b[38;5;2m   2\x1b[39m:     println!(\"hello\x1b[4m\x1b[38;5;2m world\x1b[24m\x1b[39m\");
";
        let out = run_opts(input, true);
        assert!(
            out.contains("\x1b[4m"),
            "expected underline preserved:\n{out}"
        );
        assert!(
            out.contains("\x1b[58;2;56;110;72m"),
            "expected muted green underline color:\n{out}"
        );
        // syntax color on the payload should still appear (not replaced by solid green word)
        assert!(
            out.contains("\x1b[38;2;"),
            "expected syntect fg kept:\n{out}"
        );
        let off = run(input);
        assert!(
            !off.contains("\x1b[4m"),
            "underline off by default:\n{off}"
        );
    }

    #[test]
    fn toml_lines_get_syntax_ansi() {
        let input = "\
Modified regular file Cargo.toml:
   1    1: name = \"kara\"
";
        let out = run(input);
        assert!(
            out.contains("\x1b[38;2;"),
            "expected syntect on toml:\n{out}"
        );
    }
}
