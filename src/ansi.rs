//! ANSI strip/scan helpers for classifying jj output and re-applying word-diff underlines.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffTone {
    Add,
    Remove,
}

impl DiffTone {
    /// Underline color only (SGR 58) — keeps syntect fg on the text.
    /// Tuned near ADD_BG/DEL_BG washes, lifted just enough to stay visible.
    pub fn underline_sgr(self) -> &'static str {
        match self {
            DiffTone::Add => "\x1b[4m\x1b[58;2;56;110;72m",
            DiffTone::Remove => "\x1b[4m\x1b[58;2;140;70;78m",
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

/// Payload text + underline ranges for color-words lines.
///
/// jj word-replacements emit removed then added tokens inline. When `keep_removes`
/// is false (dual-sided / add-only), skip removed-tone chars so highlighting sees
/// the right-hand line (`…highlight` + `kara` → `kara`).
pub fn color_words_display_payload(
    original: &str,
    payload_at: usize,
    keep_removes: bool,
    fallback: DiffTone,
) -> (String, Vec<(usize, usize, DiffTone)>) {
    let mut payload = String::new();
    let mut ranges = Vec::new();
    let mut plain = 0usize; // all plain chars (incl. skipped)
    let mut out_i = 0usize; // kept payload chars only
    let mut underlined = false;
    let mut start = 0usize;
    let mut tone = fallback;
    let mut span_tone = fallback;
    let mut chars = original.char_indices().peekable();

    let close_underline = |ranges: &mut Vec<_>,
                           underlined: &mut bool,
                           start: usize,
                           end: usize,
                           span_tone: DiffTone| {
        if *underlined && end > start {
            ranges.push((start, end, span_tone));
        }
        *underlined = false;
    };

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
                                    close_underline(
                                        &mut ranges,
                                        &mut underlined,
                                        start,
                                        out_i,
                                        span_tone,
                                    );
                                }
                                "4" => {
                                    if !underlined {
                                        start = out_i;
                                        span_tone = tone;
                                        underlined = true;
                                    }
                                }
                                "24" | "21" => {
                                    close_underline(
                                        &mut ranges,
                                        &mut underlined,
                                        start,
                                        out_i,
                                        span_tone,
                                    );
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
        let in_payload = plain >= payload_at;
        plain += 1;
        if !in_payload {
            continue;
        }
        if !keep_removes && tone == DiffTone::Remove {
            continue;
        }
        payload.push(c);
        out_i += 1;
    }
    close_underline(&mut ranges, &mut underlined, start, out_i, span_tone);
    (payload, ranges)
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
        let (payload, ranges) = color_words_display_payload(line, 10, false, DiffTone::Add);
        assert_eq!(payload, " pager kara");
        assert!(!payload.contains("jj-diff-syntax-highlight"));
        assert_eq!(ranges.len(), 1);
        let (s, e, tone) = ranges[0];
        assert_eq!(&payload[s..e], "kara");
        assert_eq!(tone, DiffTone::Add);
    }
}
