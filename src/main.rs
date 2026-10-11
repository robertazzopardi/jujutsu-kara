//! Filter: syntax-highlight code payloads in git-unified diffs; leave everything else alone.

mod ansi;
mod filter;
mod theme;
mod vscode_theme;

use std::io::{self, BufRead, IsTerminal};
use std::process::{Command, Stdio};

use syntect::highlighting::Theme;
use syntect::parsing::SyntaxSet;

use filter::process;
use theme::resolve_theme;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return;
    }

    let mut theme_name =
        std::env::var("JJ_DIFF_THEME").unwrap_or_else(|_| "GitHub Dark Default".into());
    let mut no_pager = false;
    let mut word_underline = false;
    let mut paths: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--theme" => {
                i += 1;
                theme_name = args.get(i).cloned().unwrap_or(theme_name);
            }
            "--no-pager" => no_pager = true,
            "--underline" => word_underline = true,
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

    let ps = two_face::syntax::extra_newlines();
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
                run_with_pager(cursor, &ps, &theme, word_underline)
            } else {
                process(cursor, &mut io::stdout().lock(), &ps, &theme, word_underline)
            }
        } else {
            let stdin = io::stdin();
            let locked = stdin.lock();
            if use_pager {
                run_with_pager(locked, &ps, &theme, word_underline)
            } else {
                process(locked, &mut io::stdout().lock(), &ps, &theme, word_underline)
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
  --underline    re-apply jj word-diff underlines (off by default)
  -h, --help     show this help
"
    );
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

fn run_with_pager(
    input: impl BufRead,
    ps: &SyntaxSet,
    theme: &Theme,
    word_underline: bool,
) -> io::Result<()> {
    let mut child = Command::new("less")
        .args(["-FRXK"])
        .stdin(Stdio::piped())
        .spawn()?;
    {
        let mut pipe = child.stdin.take().expect("piped stdin");
        process(input, &mut pipe, ps, theme, word_underline)?;
    }
    match child.wait()?.code() {
        Some(0 | 1) | None => Ok(()), // less exits 1 on some quit paths
        Some(code) => Err(io::Error::other(format!("less exited {code}"))),
    }
}

fn is_broken_pipe(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::BrokenPipe
}
