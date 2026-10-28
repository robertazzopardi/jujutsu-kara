//! Line-oriented filter: highlight code payloads in git-unified / jj color-words diffs.

use std::io::{self, BufRead, Write};

use syntect::easy::HighlightLines;
use syntect::highlighting::Theme;
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::as_24_bit_terminal_escaped;

use crate::ansi::{
    DiffTone, color_words_display_payload, overlay_word_diff, shift_underline_ranges_for_removes,
    split_ansi_at_stripped, stitch_removes, strip_ansi, underline_ranges,
};
use crate::theme::{ADD_BG, DEL_BG, sgr_bg};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Auto,
    Git,
    ColorWords,
}

/// One input line, split for output (keep CR) and matching (strip CR + ANSI).
struct Line<'a> {
    /// Original line, including a trailing `\r` if present.
    raw: &'a str,
    /// Line without a trailing `\r` (used for classification and highlighting).
    plain: &'a str,
    /// `plain` with ANSI escapes removed.
    stripped: &'a str,
    had_cr: bool,
    had_nl: bool,
}

pub fn process<'a>(
    mut input: impl BufRead,
    out: &mut impl Write,
    ps: &'a SyntaxSet,
    theme: &'a Theme,
    word_underline: bool,
) -> io::Result<()> {
    let mut mode = Mode::Auto;
    let mut syntax: Option<&'a SyntaxReference> = None;
    // True once we're inside a `@@` hunk, so `--- `/`+++ ` content lines aren't
    // mistaken for file headers.
    let mut in_hunk = false;
    let mut highlight: Option<DiffHighlight<'a>> = None;
    let mut buf = String::new();

    loop {
        buf.clear();
        let n = input.read_line(&mut buf)?;
        if n == 0 {
            break;
        }
        let had_nl = buf.ends_with('\n');
        // Keep the CR for output (passthrough must stay byte-identical), but
        // strip it for classification/highlighting.
        let raw = if had_nl {
            &buf[..buf.len() - 1]
        } else {
            buf.as_str()
        };
        let had_cr = raw.ends_with('\r');
        let plain = raw.strip_suffix('\r').unwrap_or(raw);
        let stripped = strip_ansi(plain);
        let l = Line {
            raw,
            plain,
            stripped: &stripped,
            had_cr,
            had_nl,
        };

        if l.stripped.starts_with("diff ") {
            mode = Mode::Git;
            in_hunk = false;
            syntax = path_from_diff_header(l.stripped).and_then(|p| syntax_for_path(ps, &p));
            reset_highlight(&mut highlight, syntax, theme);
            write_raw(out, l.raw, l.had_nl)?;
            continue;
        }

        if let Some(path) = path_from_color_words_header(l.stripped) {
            mode = Mode::ColorWords;
            syntax = syntax_for_path(ps, path);
            reset_highlight(&mut highlight, syntax, theme);
            write_raw(out, l.raw, l.had_nl)?;
            continue;
        }

        // Bare unified diffs (`diff -u`, no `diff ` line) still have `+++`/`@@`.
        if mode == Mode::Auto
            && (l.stripped.starts_with("@@") || path_from_plus_header(l.stripped).is_some())
        {
            mode = Mode::Git;
        }

        match mode {
            Mode::ColorWords => {
                if let Some(payload_at) = color_words_payload_at(l.stripped) {
                    write_color_words_line(
                        out,
                        &l,
                        payload_at,
                        syntax,
                        ps,
                        theme,
                        word_underline,
                        &mut highlight,
                    )?;
                } else {
                    write_raw(out, l.raw, l.had_nl)?;
                }
            }
            Mode::Git => {
                if l.stripped.starts_with("@@") {
                    in_hunk = true;
                    write_raw(out, l.raw, l.had_nl)?;
                } else if !in_hunk && l.stripped.starts_with("+++ ") {
                    if let Some(p) = path_from_plus_header(l.stripped) {
                        syntax = syntax_for_path(ps, &p);
                        reset_highlight(&mut highlight, syntax, theme);
                    }
                    write_raw(out, l.raw, l.had_nl)?;
                } else if !in_hunk && is_passthrough_git_line(l.stripped) {
                    write_raw(out, l.raw, l.had_nl)?;
                } else if let Some(marker) = content_marker(l.stripped) {
                    let payload_at = marker.len_utf8();
                    let payload = &l.stripped[payload_at..];
                    let side = match marker {
                        '-' => HighlightSide::Old,
                        '+' => HighlightSide::New,
                        ' ' => HighlightSide::Both,
                        _ => unreachable!(),
                    };
                    let mut highlighted = highlight_for(
                        &mut highlight,
                        syntax,
                        theme,
                        ps,
                        side,
                        payload,
                    );
                    if word_underline {
                        let fallback = match marker {
                            '-' => DiffTone::Remove,
                            _ => DiffTone::Add,
                        };
                        let under = underline_ranges(l.plain, payload_at, fallback);
                        if !under.is_empty() {
                            highlighted = overlay_word_diff(&highlighted, &under);
                        }
                    }
                    write_content(out, marker, &highlighted, l.had_cr, l.had_nl)?;
                } else {
                    write_raw(out, l.raw, l.had_nl)?;
                }
            }
            Mode::Auto => write_raw(out, l.raw, l.had_nl)?,
        }
    }
    out.flush()
}

struct DiffHighlight<'a> {
    old: HighlightLines<'a>,
    new: HighlightLines<'a>,
}

impl<'a> DiffHighlight<'a> {
    fn new(syntax: &'a SyntaxReference, theme: &'a Theme) -> Self {
        Self {
            old: HighlightLines::new(syntax, theme),
            new: HighlightLines::new(syntax, theme),
        }
    }
}

#[derive(Clone, Copy)]
enum HighlightSide {
    Old,
    New,
    Both,
}

fn reset_highlight<'a>(
    slot: &mut Option<DiffHighlight<'a>>,
    syntax: Option<&'a SyntaxReference>,
    theme: &'a Theme,
) {
    *slot = syntax.map(|syn| DiffHighlight::new(syn, theme));
}

fn highlight_for<'a>(
    slot: &mut Option<DiffHighlight<'a>>,
    syntax: Option<&'a SyntaxReference>,
    theme: &'a Theme,
    ps: &SyntaxSet,
    side: HighlightSide,
    payload: &str,
) -> String {
    let Some(syn) = syntax else {
        return payload.to_string();
    };
    if slot.is_none() {
        *slot = Some(DiffHighlight::new(syn, theme));
    }
    let state = slot.as_mut().expect("highlight slot");
    match side {
        HighlightSide::Old => highlight_line_payload(&mut state.old, ps, payload),
        HighlightSide::New => highlight_line_payload(&mut state.new, ps, payload),
        HighlightSide::Both => {
            let _ = highlight_line_payload(&mut state.old, ps, payload);
            highlight_line_payload(&mut state.new, ps, payload)
        }
    }
}

/// Highlight a single color-words payload line and write it with its wash.
fn write_color_words_line<'a>(
    out: &mut impl Write,
    l: &Line<'_>,
    payload_at: usize,
    syntax: Option<&'a SyntaxReference>,
    ps: &SyntaxSet,
    theme: &'a Theme,
    word_underline: bool,
    highlight: &mut Option<DiffHighlight<'a>>,
) -> io::Result<()> {
    let (prefix, _) = split_ansi_at_stripped(l.plain, payload_at);
    let bg = color_words_line_bg(l.stripped);
    // Keep jj word-diff marks: underline + red/green (not harsh syntax colors).
    let fallback = match bg {
        Some(bg) if bg == ADD_BG => DiffTone::Add,
        Some(_) => DiffTone::Remove,
        None => DiffTone::Add,
    };
    // Delete-only: keep removes in payload. Dual/add: syntect on right side,
    // then stitch removed (before) tokens back for inline before/after.
    let keep_removes = bg == Some(DEL_BG);
    let (payload, under, removes) =
        color_words_display_payload(l.plain, payload_at, keep_removes, fallback);
    let inline_change = !removes.is_empty() || !under.is_empty();
    let side = match bg {
        Some(bg) if bg == DEL_BG => HighlightSide::Old,
        Some(_) => HighlightSide::New,
        None if inline_change => HighlightSide::New,
        None => HighlightSide::Both,
    };
    let mut highlighted = highlight_for(highlight, syntax, theme, ps, side, &payload);
    if !removes.is_empty() {
        highlighted = stitch_removes(&highlighted, &removes);
    }
    if word_underline && !under.is_empty() {
        let under = shift_underline_ranges_for_removes(&under, &removes);
        highlighted = overlay_word_diff(&highlighted, &under);
    }
    write!(out, "{prefix}")?;
    if let Some(bg) = bg {
        write!(out, "{}", sgr_bg(bg))?;
    }
    write!(out, "{highlighted}\x1b[0m")?;
    write_line_ending(out, l.had_cr, l.had_nl)
}

fn write_line_ending(out: &mut impl Write, had_cr: bool, had_nl: bool) -> io::Result<()> {
    if had_cr {
        out.write_all(b"\r")?;
    }
    if had_nl {
        out.write_all(b"\n")?;
    }
    Ok(())
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
    had_cr: bool,
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
    write_line_ending(out, had_cr, had_nl)
}

fn highlight_line_payload(h: &mut HighlightLines, ps: &SyntaxSet, payload: &str) -> String {
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
    let prefix = stripped.get(..color_words_colon(stripped)?)?;
    let bytes = prefix.as_bytes();
    // jj right-aligns each column, so a present number ends in a digit.
    let right_blank = !bytes.last().is_some_and(u8::is_ascii_digit);
    let right_start = bytes
        .iter()
        .rposition(|b| !b.is_ascii_digit())
        .map_or(0, |i| i + 1);
    let left_blank = bytes[..right_start].iter().all(|b| *b == b' ');
    match (left_blank, right_blank) {
        (true, false) => Some(ADD_BG),
        (false, true) => Some(DEL_BG),
        _ => None,
    }
}

fn content_marker(stripped: &str) -> Option<char> {
    let c = stripped.chars().next()?;
    matches!(c, ' ' | '+' | '-').then_some(c)
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

/// `Modified regular file path:` / `Added … file path:` / renames.
///
/// jj renders renames/copies as `Modified regular file new (old => new):`, so
/// strip the trailing `(old => new)` annotation; older output used
/// `Renamed old => new:`.
fn path_from_color_words_header(stripped: &str) -> Option<&str> {
    let s = stripped.strip_suffix(':')?;
    let path = if let Some(i) = s.find(" file ") {
        &s[i + " file ".len()..]
    } else if let Some(rest) = s.strip_prefix("Renamed ") {
        rest
    } else {
        s.strip_prefix("Copied ")?
    };
    if path.is_empty() {
        return None;
    }
    // "new (old => new)" — keep only the new path.
    let path = match path.rfind(" (") {
        Some(open) if path[open..].contains(" => ") && path.ends_with(')') => &path[..open],
        _ => path,
    };
    // "old.rs => new.rs"
    Some(match path.rsplit_once(" => ") {
        Some((_, new)) => new,
        None => path,
    })
}

/// Offset of the `:` that terminates jj's line-number columns, if this is a
/// color-words line.
///
/// jj writes `{left:>4} {right:>4}: ` but each `{:>4}` is a *minimum* width, so
/// columns widen past 9999 (e.g. `10016 10016: `). Parse the colon instead of
/// assuming fixed offsets.
fn color_words_colon(stripped: &str) -> Option<usize> {
    let colon = stripped.find(':')?;
    let prefix = stripped.as_bytes().get(..colon)?;
    if !prefix.iter().any(u8::is_ascii_digit) {
        return None;
    }
    if !prefix.iter().all(|b| b.is_ascii_digit() || *b == b' ') {
        return None;
    }
    Some(colon)
}

/// Byte offset of the code payload on a color-words line (the space after `:`).
fn color_words_payload_at(stripped: &str) -> Option<usize> {
    Some(color_words_colon(stripped)? + 1)
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
        assert!(!off.contains("\x1b[4m"), "underline off by default:\n{off}");
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

    #[test]
    fn content_lines_shaped_like_headers_are_highlighted() {
        // A removed SQL comment `-- old` renders as `--- old`, colliding with
        // the `--- a/file` file header; an added `++ x` renders as `+++ x`.
        let input = "\
diff --git a/x.sql b/x.sql
--- a/x.sql
+++ b/x.sql
@@ -1,2 +1,2 @@
--- old comment
+SELECT 1;
 SELECT 2;
";
        let out = run(input);
        let removed = out
            .lines()
            .find(|l| l.contains("old comment"))
            .expect("removed line present");
        assert!(
            removed.contains(&sgr_bg(DEL_BG)),
            "removed line lost its wash:\n{out}"
        );
        assert!(
            removed.contains("\x1b[38;2;"),
            "removed line lost syntax colors:\n{out}"
        );
    }

    #[test]
    fn added_plus_line_does_not_clear_syntax() {
        // Added `++ added` renders as `+++ added`; it must not be read as a
        // `+++ b/file` header (which would reset syntax for the rest of file).
        let input = "\
diff --git a/x.js b/x.js
--- a/x.js
+++ b/x.js
@@ -1,2 +1,4 @@
 fn a() {}
+++ added
+let q = 9;
 fn b() {}
";
        let out = run(input);
        let added = out
            .lines()
            .find(|l| l.contains("added"))
            .expect("added line present");
        assert!(
            added.contains(&sgr_bg(ADD_BG)),
            "added line lost wash:\n{out}"
        );
        let context = out
            .lines()
            .find(|l| strip_ansi(l).contains("fn b"))
            .expect("context line present");
        assert!(
            context.contains("\x1b[38;2;"),
            "syntax state was reset after `+++` content line:\n{out}"
        );
    }

    #[test]
    fn rename_header_keeps_syntax() {
        let input = "\
Modified regular file new.rs (old.rs => new.rs):
   1    1: fn main() {}
";
        let out = run(input);
        assert!(
            out.contains("\x1b[38;2;"),
            "rename annotation cleared the syntax:\n{out}"
        );
    }

    #[test]
    fn crlf_line_endings_preserved() {
        let input = "diff --git a/x.rs b/x.rs\r\n--- a/x.rs\r\n+++ b/x.rs\r\n@@ -1 +1 @@\r\n-let a = 1;\r\n+let a = 2;\r\n";
        let out = run(input);
        assert!(
            out.contains("diff --git a/x.rs b/x.rs\r\n"),
            "passthrough CRLF lost:\n{out:?}"
        );
        assert!(out.contains("--- a/x.rs\r\n"), "header CRLF lost:\n{out:?}");
        assert!(out.contains("\x1b[0m\r\n"), "content CRLF lost:\n{out:?}");
        assert!(out.ends_with("\r\n"), "final CRLF lost:\n{out:?}");
    }

    #[test]
    fn bare_unified_diff_is_highlighted() {
        // No `diff ` line (plain `diff -u` output) still gets highlighted.
        let input = "\
--- a/x.rs
+++ b/x.rs
@@ -1 +1 @@
-fn old() {}
+fn new() {}
";
        let out = run(input);
        assert!(
            out.contains("\x1b[38;2;"),
            "bare unified diff not highlighted:\n{out}"
        );
        assert!(out.contains(&sgr_bg(ADD_BG)), "no add wash:\n{out}");
        assert!(out.contains(&sgr_bg(DEL_BG)), "no del wash:\n{out}");
    }

    #[test]
    fn color_words_wide_line_numbers() {
        let input = "\
Modified regular file big.rs:
    ...
10018 10018: fn a() {}
10019     : fn b() {}
     10019: fn c() {}
";
        let out = run(input);
        let deleted = out
            .lines()
            .find(|l| strip_ansi(l).contains("fn b"))
            .unwrap();
        assert!(
            deleted.contains(&sgr_bg(DEL_BG)),
            "wide deleted line lost wash:\n{out}"
        );
        let added = out
            .lines()
            .find(|l| strip_ansi(l).contains("fn c"))
            .unwrap();
        assert!(
            added.contains(&sgr_bg(ADD_BG)),
            "wide added line lost wash:\n{out}"
        );
        assert!(
            added.contains("\x1b[38;2;"),
            "wide columns stopped syntax highlighting:\n{out}"
        );
    }

    #[test]
    fn color_words_inline_word_replace_keeps_syntax() {
        let input = "\
Modified regular file foo.rs:
\x1b[38;5;1m   1\x1b[39m \x1b[38;5;2m   1\x1b[39m: let x = \"\x1b[4m\x1b[38;5;1mexampleExample\x1b[38;5;2mexample\x1b[24m\x1b[39m\";
";
        let out = run(input);
        let line = out.lines().nth(1).expect("diff line");
        let plain = strip_ansi(line);
        assert!(
            plain.contains("exampleExampleexample"),
            "jj inline remove+add text missing:\n{plain}\n{out}"
        );
        // String literal color must survive past the stitched removed token.
        let after_removed = line.split("exampleExample").nth(1).expect("removed span");
        assert!(
            after_removed.starts_with("\x1b[24m\x1b[38;2;165;214;255mexample"),
            "string syntax color lost on kept token after inline remove:\n{out}"
        );
    }

    #[test]
    fn color_words_middle_of_string_replace_keeps_prefix_suffix_colors() {
        let input = "\
Modified regular file foo.rs:
\x1b[38;5;1m   3\x1b[39m \x1b[38;5;2m   3\x1b[39m:     let y = \"\x1b[4m\x1b[38;5;1mprefixExampleSuffix\x1b[38;5;2mprefixsuffix\x1b[24m\x1b[39m\";
";
        let out = run(input);
        let line = out.lines().nth(1).expect("diff line");
        let after_removed = line.split("prefixExampleSuffix").nth(1).expect("removed span");
        assert!(
            after_removed.starts_with("\x1b[24m\x1b[38;2;165;214;255mprefixsuffix"),
            "suffix lost string fg:\n{out}"
        );
    }

    #[test]
    fn color_words_inline_word_replace_keeps_underline() {
        let input = "\
Modified regular file foo.rs:
\x1b[38;5;1m   1\x1b[39m \x1b[38;5;2m   1\x1b[39m: let x = \"\x1b[4m\x1b[38;5;1mexampleExample\x1b[38;5;2mexample\x1b[24m\x1b[39m\";
";
        let out = run_opts(input, true);
        let line = out.lines().nth(1).expect("diff line");
        assert!(
            line.contains("\x1b[58;2;56;110;72m"),
            "add underline missing on kept token after stitch:\n{out}"
        );
    }

    #[test]
    fn color_words_multiline_state_keeps_comment_highlight() {
        let input = "\
Modified regular file foo.rs:
   1    1: fn demo() {
   2    2:     /* start
        3:     still comment */
   3    4:     let x = \"\x1b[4m\x1b[38;5;1mexampleExample\x1b[38;5;2mexample\x1b[24m\x1b[39m\";
   4    5: }
";
        let out = run(input);
        let comment = out
            .lines()
            .find(|l| strip_ansi(l).contains("still comment"))
            .expect("comment continuation line");
        assert!(
            comment.contains("\x1b[38;2;"),
            "comment line lost syntax colors:\n{out}"
        );
    }

    #[test]
    fn color_words_columns_wide() {
        assert_eq!(color_words_payload_at("10016 10016: let x"), Some(12));
        assert_eq!(color_words_payload_at("10019     : let x"), Some(11));
        assert_eq!(color_words_payload_at("     10019: let x"), Some(11));
        assert_eq!(color_words_line_bg("10016 10016: x"), None);
        assert_eq!(color_words_line_bg("10019     : x"), Some(DEL_BG));
        assert_eq!(color_words_line_bg("     10019: x"), Some(ADD_BG));
    }
}
