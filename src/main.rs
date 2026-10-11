//! Filter: syntax-highlight code payloads in git-unified diffs; leave everything else alone.

use std::io::{self, BufRead, IsTerminal, Write};
use std::process::{Command, Stdio};

use syntect::easy::HighlightLines;
use syntect::highlighting::Theme;
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::as_24_bit_terminal_escaped;
use two_face::theme::{LazyThemeSet, extra};
use vscode_theme_syntect::parse_vscode_theme;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return;
    }

    let mut theme_name =
        std::env::var("JJ_DIFF_THEME").unwrap_or_else(|_| "GitHub Dark Default".into());
    let mut no_pager = false;
    let mut paths: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--theme" => {
                i += 1;
                theme_name = args.get(i).cloned().unwrap_or(theme_name);
            }
            "--no-pager" => no_pager = true,
            other if other.starts_with('-') => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
            other => paths.push(other.to_string()),
        }
        i += 1;
    }

    if paths.len() > 2 {
        eprintln!("usage: kara [options] [LEFT RIGHT]");
        std::process::exit(2);
    }

    let ps = SyntaxSet::load_defaults_newlines();
    let theme = resolve_theme(&theme_name);

    // jj --tool / ui.diff-formatter invokes us with $left $right directories; don't page (jj pages).
    let tool_mode = paths.len() == 2;
    if tool_mode {
        no_pager = true;
    }
    let use_pager = !no_pager && stdout_is_tty();

    let result = (|| -> io::Result<()> {
        if tool_mode {
            let diff = unified_diff(&paths[0], &paths[1])?;
            let cursor = io::Cursor::new(diff);
            if use_pager {
                run_with_pager(cursor, &ps, &theme)
            } else {
                process(cursor, &mut io::stdout().lock(), &ps, &theme)
            }
        } else {
            let stdin = io::stdin();
            let locked = stdin.lock();
            if use_pager {
                run_with_pager(locked, &ps, &theme)
            } else {
                process(locked, &mut io::stdout().lock(), &ps, &theme)
            }
        }
    })();

    if let Err(e) = result {
        if is_broken_pipe(&e) {
            return;
        }
        eprintln!("kara: {e}");
        std::process::exit(1);
    }
}

fn print_help() {
    print!(
        "\
kara — syntax-highlight code in jj diffs

Preferred (keeps jj color-words):
  jj config unset --user ui.diff-formatter   # or :color-words
  jj config set --user ui.pager kara

Also supports git-unified on stdin, and jj --tool LEFT RIGHT (unified only).

Options:
  --theme NAME   GitHub Dark Default (default), or any two-face name / $JJ_DIFF_THEME
  --no-pager     never spawn less (default when stdout is not a TTY, or tool mode)
  -h, --help     show this help
"
    );
}

/// Dark green / red washes (IDE-style), truecolor.
const ADD_BG: (u8, u8, u8) = (18, 42, 28);
const DEL_BG: (u8, u8, u8) = (52, 24, 28);

fn theme_set() -> LazyThemeSet {
    LazyThemeSet::from(extra())
}

/// Primer GitHub Dark Default — always dark fg/bg token colors (truecolor), independent of terminal theme.
fn github_dark_default() -> Theme {
    let vs = parse_vscode_theme(include_str!("../assets/github-dark-default.json"))
        .expect("embedded GitHub Dark Default JSON");
    Theme::try_from(vs).expect("convert GitHub Dark Default to syntect")
}

fn is_github_dark_alias(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "github" | "github dark" | "github dark default" | "github-dark" | "github-dark-default"
    )
}

fn resolve_theme(name: &str) -> Theme {
    if is_github_dark_alias(name) {
        return github_dark_default();
    }
    let themes = theme_set();
    themes.get(name).cloned().unwrap_or_else(|| {
        eprintln!("unknown theme '{name}', falling back to GitHub Dark Default");
        github_dark_default()
    })
}

fn sgr_bg((r, g, b): (u8, u8, u8)) -> String {
    format!("\x1b[48;2;{r};{g};{b}m")
}

/// jj --tool passes directory trees; produce a unified diff (exit 1 from diff = changes, OK).
fn unified_diff(left: &str, right: &str) -> io::Result<Vec<u8>> {
    let output = Command::new("diff").args(["-ruN", left, right]).output()?;
    // 0 = identical, 1 = different, 2 = error
    if output.status.code() == Some(2)
        || (!output.status.success() && output.status.code() != Some(1))
    {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(format!(
            "diff -ruN {left} {right} failed: {stderr}"
        )));
    }
    Ok(output.stdout)
}

fn stdout_is_tty() -> bool {
    io::stdout().is_terminal()
}

fn run_with_pager(input: impl BufRead, ps: &SyntaxSet, theme: &Theme) -> io::Result<()> {
    let mut child = Command::new("less")
        .args(["-FRXK"])
        .stdin(Stdio::piped())
        .spawn()?;
    {
        let mut pipe = child.stdin.take().expect("piped stdin");
        process(input, &mut pipe, ps, theme)?;
    }
    match child.wait()?.code() {
        Some(0) | Some(1) | None => Ok(()), // less exits 1 on some quit paths
        Some(code) => Err(io::Error::other(format!("less exited {code}"))),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Auto,
    Git,
    ColorWords,
}

fn process(
    mut input: impl BufRead,
    out: &mut impl Write,
    ps: &SyntaxSet,
    theme: &Theme,
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
                    let payload = &stripped[payload_at..];
                    let mut highlighted = match syntax {
                        Some(syn) => highlight_payload(ps, theme, syn, payload),
                        None => payload.to_string(),
                    };
                    // Keep jj word-diff marks: underline + red/green (not harsh syntax colors).
                    let fallback = match color_words_line_bg(&stripped) {
                        Some(bg) if bg == ADD_BG => DiffTone::Add,
                        Some(_) => DiffTone::Remove,
                        None => DiffTone::Add,
                    };
                    let under = underline_ranges(line, payload_at, fallback);
                    if !under.is_empty() {
                        highlighted = overlay_word_diff(&highlighted, &under);
                    }
                    let bg = color_words_line_bg(&stripped);
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
                    let mut highlighted = match syntax {
                        Some(syn) => highlight_payload(ps, theme, syn, payload),
                        None => payload.to_string(),
                    };
                    let fallback = match marker {
                        '+' => DiffTone::Add,
                        '-' => DiffTone::Remove,
                        _ => DiffTone::Add,
                    };
                    let under = underline_ranges(line, payload_at, fallback);
                    if !under.is_empty() {
                        highlighted = overlay_word_diff(&highlighted, &under);
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
    match h.highlight_line(line, ps) {
        Ok(ranges) => {
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
        }
        Err(_) => payload.to_string(),
    }
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
    } else if let Some(rest) = s.strip_prefix("Copied ") {
        rest
    } else {
        return None;
    };
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiffTone {
    Add,
    Remove,
}

impl DiffTone {
    /// Underline color only (SGR 58) — keeps syntect fg on the text.
    /// Tuned near ADD_BG/DEL_BG washes, lifted just enough to stay visible.
    fn underline_sgr(self) -> &'static str {
        match self {
            DiffTone::Add => "\x1b[4m\x1b[58;2;56;110;72m",
            DiffTone::Remove => "\x1b[4m\x1b[58;2;140;70;78m",
        }
    }
}

/// Plain-char ranges `[start, end)` relative to `payload_at` that jj underlined.
fn underline_ranges(
    original: &str,
    payload_at: usize,
    fallback: DiffTone,
) -> Vec<(usize, usize, DiffTone)> {
    underline_ranges_abs(original, fallback)
        .into_iter()
        .filter_map(|(s, e, tone)| {
            let s = s.max(payload_at);
            let e = e.max(payload_at);
            (e > s).then_some((s - payload_at, e - payload_at, tone))
        })
        .collect()
}

fn underline_ranges_abs(original: &str, fallback: DiffTone) -> Vec<(usize, usize, DiffTone)> {
    let mut ranges = Vec::new();
    let mut plain = 0usize;
    let mut underlined = false;
    let mut start = 0usize;
    let mut tone = fallback;
    let mut span_tone = fallback;
    let mut chars = original.char_indices().peekable();

    while let Some((_, c)) = chars.next() {
        if c == '\u{1b}' {
            match chars.peek().map(|(_, c)| *c) {
                Some('[') => {
                    chars.next();
                    let mut params = String::new();
                    let mut final_byte = 'm';
                    for (_, c2) in chars.by_ref() {
                        if ('@'..='~').contains(&c2) {
                            final_byte = c2;
                            break;
                        }
                        params.push(c2);
                    }
                    if final_byte == 'm' {
                        if let Some(t) = tone_from_sgr(&params) {
                            tone = t;
                            if underlined {
                                span_tone = t;
                            }
                        }
                        for p in params.split(';') {
                            match p {
                                "" | "0" => {
                                    if underlined {
                                        ranges.push((start, plain, span_tone));
                                        underlined = false;
                                    }
                                }
                                "4" => {
                                    if !underlined {
                                        start = plain;
                                        span_tone = tone;
                                        underlined = true;
                                    }
                                }
                                "24" | "21" => {
                                    if underlined {
                                        ranges.push((start, plain, span_tone));
                                        underlined = false;
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some((_, c2)) = chars.next() {
                        if c2 == '\u{7}' {
                            break;
                        }
                        if c2 == '\u{1b}' && chars.peek().map(|(_, c)| *c) == Some('\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
            continue;
        }
        plain += 1;
    }
    if underlined {
        ranges.push((start, plain, span_tone));
    }
    ranges
}

fn tone_from_sgr(params: &str) -> Option<DiffTone> {
    let parts: Vec<&str> = params.split(';').filter(|p| !p.is_empty()).collect();
    // 31/91 red, 32/92 green
    for p in &parts {
        match *p {
            "31" | "91" => return Some(DiffTone::Remove),
            "32" | "92" => return Some(DiffTone::Add),
            _ => {}
        }
    }
    // 38;5;n — jj defaults: 1 red, 2 green
    if let Some(i) = parts.iter().position(|&p| p == "38") {
        if parts.get(i + 1) == Some(&"5") {
            if let Some(n) = parts.get(i + 2).and_then(|s| s.parse::<u8>().ok()) {
                return match n {
                    1 | 9 => Some(DiffTone::Remove),
                    2 | 10 => Some(DiffTone::Add),
                    _ => None,
                };
            }
        }
        if parts.get(i + 1) == Some(&"2") {
            if let (Some(r), Some(g), Some(_b)) = (
                parts.get(i + 2).and_then(|s| s.parse::<u8>().ok()),
                parts.get(i + 3).and_then(|s| s.parse::<u8>().ok()),
                parts.get(i + 4).and_then(|s| s.parse::<u8>().ok()),
            ) {
                return if g > r {
                    Some(DiffTone::Add)
                } else if r > g {
                    Some(DiffTone::Remove)
                } else {
                    None
                };
            }
        }
    }
    None
}

/// Colored underline on jj word-diff spans; syntect foreground stays as-is.
fn overlay_word_diff(highlighted: &str, ranges: &[(usize, usize, DiffTone)]) -> String {
    if ranges.is_empty() {
        return highlighted.to_string();
    }
    let mut out = String::with_capacity(highlighted.len() + ranges.len() * 16);
    let mut plain = 0usize;
    let mut active: Option<DiffTone> = None;
    let mut chars = highlighted.char_indices().peekable();

    while let Some((_, c)) = chars.next() {
        if c == '\u{1b}' {
            out.push(c);
            match chars.peek().map(|(_, c)| *c) {
                Some('[') => {
                    out.push(chars.next().unwrap().1);
                    let mut final_byte = 'm';
                    for (_, c2) in chars.by_ref() {
                        out.push(c2);
                        if ('@'..='~').contains(&c2) {
                            final_byte = c2;
                            break;
                        }
                    }
                    // SGR can clear underline; re-assert style + underline color only.
                    if final_byte == 'm' {
                        if let Some(tone) = active {
                            out.push_str(tone.underline_sgr());
                        }
                    }
                }
                Some(']') => {
                    out.push(chars.next().unwrap().1);
                    while let Some((_, c2)) = chars.next() {
                        out.push(c2);
                        if c2 == '\u{7}' {
                            break;
                        }
                        if c2 == '\u{1b}' {
                            if let Some((_, '\\')) = chars.peek().copied() {
                                out.push(chars.next().unwrap().1);
                                break;
                            }
                        }
                    }
                }
                Some(_) => {
                    out.push(chars.next().unwrap().1);
                }
                None => {}
            }
            continue;
        }
        let want = ranges
            .iter()
            .find(|&&(s, e, _)| plain >= s && plain < e)
            .map(|&(_, _, t)| t);
        match (active, want) {
            (None, Some(tone)) => {
                out.push_str(tone.underline_sgr());
                active = Some(tone);
            }
            (Some(_), None) => {
                out.push_str("\x1b[24m\x1b[59m"); // underline off + default underline color
                active = None;
            }
            (Some(a), Some(b)) if a != b => {
                out.push_str("\x1b[24m\x1b[59m");
                out.push_str(b.underline_sgr());
                active = Some(b);
            }
            _ => {}
        }
        out.push(c);
        plain += 1;
    }
    if active.is_some() {
        out.push_str("\x1b[24m\x1b[59m");
    }
    out
}

/// Split `original` so the left part covers the first `plain_len` non-ANSI chars.
fn split_ansi_at_stripped(original: &str, plain_len: usize) -> (&str, &str) {
    let mut plain = 0usize;
    let mut chars = original.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '\u{1b}' {
            match chars.peek().map(|(_, c)| *c) {
                Some('[') => {
                    chars.next();
                    for (_, c2) in chars.by_ref() {
                        if ('@'..='~').contains(&c2) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some((_, c2)) = chars.next() {
                        if c2 == '\u{7}' {
                            break;
                        }
                        if c2 == '\u{1b}' && chars.peek().map(|(_, c)| *c) == Some('\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
            continue;
        }
        if plain == plain_len {
            return (&original[..i], &original[i..]);
        }
        plain += 1;
    }
    (original, "")
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
    if let Some(syn) =
        ps.find_syntax_by_extension(name.rsplit_once('.').map(|(_, e)| e).unwrap_or(""))
    {
        return Some(syn);
    }
    ps.find_syntax_by_name(name)
        .or_else(|| ps.find_syntax_by_extension(name))
}

/// Strip CSI / OSC ANSI sequences for classification.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    for c2 in chars.by_ref() {
                        if ('@'..='~').contains(&c2) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    // OSC: ESC ] ... BEL or ST
                    chars.next();
                    while let Some(c2) = chars.next() {
                        if c2 == '\u{7}' {
                            break;
                        }
                        if c2 == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn is_broken_pipe(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::BrokenPipe
}

#[cfg(test)]
mod tests {
    use super::*;
    use syntect::parsing::SyntaxSet;

    fn run(input: &str) -> String {
        let ps = SyntaxSet::load_defaults_newlines();
        let theme = resolve_theme("GitHub Dark Default");
        let mut out = Vec::new();
        process(input.as_bytes(), &mut out, &ps, &theme).unwrap();
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
        let out = run(input);
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
    }

    #[test]
    fn underline_ranges_detects_jj_span() {
        let line = "   2    2:     println!(\"hello\x1b[4m\x1b[38;5;2m world\x1b[24m\x1b[39m\");";
        let ranges = underline_ranges(line, 10, DiffTone::Add);
        assert!(!ranges.is_empty(), "ranges={ranges:?}");
        let payload = "     println!(\"hello world\");";
        let (s, e, tone) = ranges[0];
        assert_eq!(&payload[s..e], " world");
        assert_eq!(tone, DiffTone::Add);
    }

    #[test]
    fn strip_ansi_basic() {
        assert_eq!(strip_ansi("\x1b[31mhi\x1b[0m"), "hi");
        assert_eq!(strip_ansi("\x1b[38;5;2m+\x1b[39mcode"), "+code");
    }
}
