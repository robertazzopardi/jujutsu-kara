//! ANSI strip/scan helpers for classifying jj output and re-applying word-diff underlines.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffTone {
    Add,
    Remove,
}

impl DiffTone {
    /// Underline color only (SGR 58) — keeps syntect fg on the text.
    /// Tuned near `ADD_BG/DEL_BG` washes, lifted just enough to stay visible.
    pub const fn underline_sgr(self) -> &'static str {
        match self {
            Self::Add => "\x1b[4m\x1b[58;2;56;110;72m",
            Self::Remove => "\x1b[4m\x1b[58;2;140;70;78m",
        }
    }
}

/// Strip CSI / OSC ANSI sequences for classification.
pub fn strip_ansi(s: &str) -> String {
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

/// Split `original` so the left part covers the first `plain_len` non-ANSI chars.
pub fn split_ansi_at_stripped(original: &str, plain_len: usize) -> (&str, &str) {
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

/// Plain-char ranges `[start, end)` relative to `payload_at` that jj underlined.
pub fn underline_ranges(
    original: &str,
    payload_at: usize,
    fallback: DiffTone,
) -> Vec<(usize, usize, DiffTone)> {
    color_words_display_payload(original, payload_at, true, fallback).1
}

/// `(right_text, underline_ranges, remove_inserts)` for [`stitch_removes`].
type ColorWordsParts = (String, Vec<(usize, usize, DiffTone)>, Vec<(usize, String)>);

/// Right-side payload + underline ranges (+ removed spans to stitch back for display).
///
/// jj word-replacements emit removed then added tokens inline. When `keep_removes`
/// is false, removed chars are omitted from the payload (so syntect sees the right
/// line) and returned as `(plain_index, text)` inserts for [`stitch_removes`].
pub fn color_words_display_payload(
    original: &str,
    payload_at: usize,
    keep_removes: bool,
    fallback: DiffTone,
) -> ColorWordsParts {
    let mut payload = String::new();
    let mut ranges = Vec::new();
    let mut removes: Vec<(usize, String)> = Vec::new();
    let mut plain = 0usize; // all plain chars (incl. skipped)
    let mut out_i = 0usize; // kept payload chars only
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
                        } else if sgr_clears_fg(&params) {
                            // jj: `\x1b[38;5;1m…\x1b[39m` then uncolored text — without
                            // this, Remove sticks and keep_removes=false drops the rest.
                            tone = fallback;
                        }
                        apply_underline_sgr(
                            &params,
                            &mut ranges,
                            &mut underlined,
                            &mut start,
                            out_i,
                            tone,
                            &mut span_tone,
                        );
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
        let in_payload = plain >= payload_at;
        plain += 1;
        if !in_payload {
            continue;
        }
        if !keep_removes && tone == DiffTone::Remove {
            match removes.last_mut() {
                Some((i, s)) if *i == out_i => s.push(c),
                _ => removes.push((out_i, c.to_string())),
            }
            continue;
        }
        payload.push(c);
        out_i += 1;
    }
    close_underline(&mut ranges, &mut underlined, start, out_i, span_tone);
    (payload, ranges, removes)
}

/// Re-insert jj removed-tone spans into highlighted right-side text at plain offsets.
pub fn stitch_removes(highlighted: &str, removes: &[(usize, String)]) -> String {
    if removes.is_empty() {
        return highlighted.to_string();
    }
    let mut out = String::with_capacity(
        highlighted.len() + removes.iter().map(|(_, s)| s.len() + 16).sum::<usize>(),
    );
    let mut plain = 0usize;
    let mut ri = 0usize;
    let mut active_fg: Option<String> = None;
    let mut chars = highlighted.char_indices().peekable();

    let flush_inserts = |out: &mut String, at: usize, ri: &mut usize, fg: &Option<String>| {
        while *ri < removes.len() && removes[*ri].0 == at {
            out.push_str("\x1b[4m\x1b[38;5;1m");
            out.push_str(&removes[*ri].1);
            out.push_str("\x1b[24m");
            if let Some(seq) = fg {
                out.push_str(seq);
            }
            *ri += 1;
        }
    };

    while let Some((_, c)) = chars.next() {
        if c == '\u{1b}' {
            out.push(c);
            match chars.peek().map(|(_, c)| *c) {
                Some('[') => {
                    out.push(chars.next().unwrap().1);
                    let mut params = String::new();
                    let mut final_byte = 'm';
                    for (_, c2) in chars.by_ref() {
                        out.push(c2);
                        if ('@'..='~').contains(&c2) {
                            final_byte = c2;
                            break;
                        }
                        params.push(c2);
                    }
                    if final_byte == 'm' {
                        apply_fg_sgr(&params, &mut active_fg);
                    }
                }
                Some(']') => {
                    out.push(chars.next().unwrap().1);
                    while let Some((_, c2)) = chars.next() {
                        out.push(c2);
                        if c2 == '\u{7}' {
                            break;
                        }
                        if c2 == '\u{1b}' && chars.peek().map(|(_, c)| *c) == Some('\\') {
                            out.push(chars.next().unwrap().1);
                            break;
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
        flush_inserts(&mut out, plain, &mut ri, &active_fg);
        out.push(c);
        plain += 1;
    }
    flush_inserts(&mut out, plain, &mut ri, &active_fg);
    out
}

/// Shift jj underline ranges after [`stitch_removes`] inserts text at plain offsets.
pub fn shift_underline_ranges_for_removes(
    ranges: &[(usize, usize, DiffTone)],
    removes: &[(usize, String)],
) -> Vec<(usize, usize, DiffTone)> {
    if removes.is_empty() {
        return ranges.to_vec();
    }
    ranges
        .iter()
        .map(|&(s, e, t)| {
            let shift: usize = removes
                .iter()
                .filter(|(i, _)| *i <= s)
                .map(|(_, text)| text.len())
                .sum();
            (s + shift, e + shift, t)
        })
        .collect()
}

fn close_underline(
    ranges: &mut Vec<(usize, usize, DiffTone)>,
    underlined: &mut bool,
    start: usize,
    end: usize,
    span_tone: DiffTone,
) {
    if *underlined && end > start {
        ranges.push((start, end, span_tone));
    }
    *underlined = false;
}

fn apply_underline_sgr(
    params: &str,
    ranges: &mut Vec<(usize, usize, DiffTone)>,
    underlined: &mut bool,
    start: &mut usize,
    out_i: usize,
    tone: DiffTone,
    span_tone: &mut DiffTone,
) {
    for p in params.split(';') {
        match p {
            "" | "0" | "24" | "21" => {
                close_underline(ranges, underlined, *start, out_i, *span_tone);
            }
            "4" if !*underlined => {
                *start = out_i;
                *span_tone = tone;
                *underlined = true;
            }
            _ => {}
        }
    }
}

fn sgr_clears_fg(params: &str) -> bool {
    params.is_empty() || params.split(';').any(|p| p == "0" || p == "39")
}

/// Full SGR sequence for a foreground-only CSI `params` (without ESC prefix).
fn fg_sgr_sequence(params: &str) -> Option<String> {
    if sgr_clears_fg(params) {
        return None;
    }
    let parts: Vec<&str> = params.split(';').filter(|p| !p.is_empty()).collect();
    if parts.first() == Some(&"38") {
        if parts.get(1) == Some(&"2") && parts.len() >= 5 {
            return Some(format!("\x1b[{params}m"));
        }
        if parts.get(1) == Some(&"5") && parts.len() >= 3 {
            return Some(format!("\x1b[{params}m"));
        }
    }
    for p in &parts {
        if matches!(*p, "30" | "31" | "32" | "33" | "34" | "35" | "36" | "37" | "90" | "91" | "92" | "93" | "94" | "95" | "96" | "97") {
            return Some(format!("\x1b[{params}m"));
        }
    }
    None
}

fn apply_fg_sgr(params: &str, active: &mut Option<String>) {
    if sgr_clears_fg(params) {
        *active = None;
        return;
    }
    if let Some(seq) = fg_sgr_sequence(params) {
        *active = Some(seq);
    }
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
        if parts.get(i + 1) == Some(&"5")
            && let Some(n) = parts.get(i + 2).and_then(|s| s.parse::<u8>().ok())
        {
            return match n {
                1 | 9 => Some(DiffTone::Remove),
                2 | 10 => Some(DiffTone::Add),
                _ => None,
            };
        }
        if parts.get(i + 1) == Some(&"2")
            && let (Some(r), Some(g), Some(_b)) = (
                parts.get(i + 2).and_then(|s| s.parse::<u8>().ok()),
                parts.get(i + 3).and_then(|s| s.parse::<u8>().ok()),
                parts.get(i + 4).and_then(|s| s.parse::<u8>().ok()),
            )
        {
            return match g.cmp(&r) {
                std::cmp::Ordering::Greater => Some(DiffTone::Add),
                std::cmp::Ordering::Less => Some(DiffTone::Remove),
                std::cmp::Ordering::Equal => None,
            };
        }
    }
    None
}

/// Colored underline on jj word-diff spans; syntect foreground stays as-is.
pub fn overlay_word_diff(highlighted: &str, ranges: &[(usize, usize, DiffTone)]) -> String {
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
                    if final_byte == 'm'
                        && let Some(tone) = active
                    {
                        out.push_str(tone.underline_sgr());
                    }
                }
                Some(']') => {
                    out.push(chars.next().unwrap().1);
                    while let Some((_, c2)) = chars.next() {
                        out.push(c2);
                        if c2 == '\u{7}' {
                            break;
                        }
                        if c2 == '\u{1b}'
                            && let Some((_, '\\')) = chars.peek().copied()
                        {
                            out.push(chars.next().unwrap().1);
                            break;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_basic() {
        assert_eq!(strip_ansi("\x1b[31mhi\x1b[0m"), "hi");
        assert_eq!(strip_ansi("\x1b[38;5;2m+\x1b[39mcode"), "+code");
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
    fn color_words_drops_removed_half_of_word_replace() {
        // jj inlines old (red) + new (green) under one underline
        let line = "   1    1: pager \x1b[4m\x1b[38;5;1mjj-diff-syntax-highlight\x1b[38;5;2mkara\x1b[24m\x1b[39m";
        let (payload, ranges, removes) =
            color_words_display_payload(line, 10, false, DiffTone::Add);
        assert_eq!(payload, " pager kara");
        assert!(!payload.contains("jj-diff-syntax-highlight"));
        assert_eq!(removes, vec![(7, "jj-diff-syntax-highlight".into())]);
        assert_eq!(ranges.len(), 1);
        let (s, e, tone) = ranges[0];
        assert_eq!(&payload[s..e], "kara");
        assert_eq!(tone, DiffTone::Add);
        let shown = stitch_removes(&payload, &removes);
        assert!(strip_ansi(&shown).contains("jj-diff-syntax-highlightkara"));
    }

    #[test]
    fn color_words_word_replace_shorter() {
        let line = "   1    1: let x = \"\x1b[4m\x1b[38;5;1mexampleExample\x1b[38;5;2mexample\x1b[24m\x1b[39m\";";
        let (payload, ranges, removes) =
            color_words_display_payload(line, 10, false, DiffTone::Add);
        assert_eq!(payload, " let x = \"example\";");
        assert_eq!(removes, vec![(10, "exampleExample".into())]);
        let (s, e, tone) = ranges[0];
        assert_eq!(&payload[s..e], "example");
        assert_eq!(tone, DiffTone::Add);
        let shown = stitch_removes(&payload, &removes);
        assert_eq!(
            strip_ansi(&shown),
            " let x = \"exampleExampleexample\";"
        );
    }

    #[test]
    fn stitch_removes_restores_active_syntax_fg() {
        let highlighted = "\x1b[38;2;165;214;255mexample";
        let removes = vec![(0, "OLD".into())];
        let out = stitch_removes(highlighted, &removes);
        assert_eq!(strip_ansi(&out), "OLDexample");
        let after_old = out.split("OLD").nth(1).expect("removed text");
        assert!(
            after_old.starts_with("\x1b[24m\x1b[38;2;165;214;255mexample"),
            "kept suffix lost string fg: {after_old:?}"
        );
    }

    #[test]
    fn color_words_keeps_uncolored_after_removed_indent() {
        // rustfmt-style: only indent spaces are red+underline; code after \x1b[39m is plain
        let line = " 249  250:             \x1b[4m\x1b[38;5;1m    \x1b[24m\x1b[39mreturn match n {";
        let (payload, _, removes) = color_words_display_payload(line, 10, false, DiffTone::Add);
        assert_eq!(payload, "             return match n {");
        assert_eq!(removes, vec![(13, "    ".into())]);
        assert_eq!(
            strip_ansi(&stitch_removes(&payload, &removes)),
            "                 return match n {"
        );
    }
}
