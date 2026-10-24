//! Filter: syntax-highlight code payloads in git-unified diffs; leave everything else alone.

mod ansi;
mod cli;
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
    let cli = cli::parse(std::env::args().skip(1));
    let ps = two_face::syntax::extra_newlines();
    let theme = resolve_theme(&cli.theme);

    if let Err(e) = run(&cli, &ps, &theme) {
        if is_broken_pipe(&e) {
            return;
        }
        eprintln!("kara: {e}");
        std::process::exit(1);
    }
}

/// Read the diff (from `LEFT RIGHT` dirs, or stdin) and highlight it to stdout or `less`.
fn run(cli: &cli::Args, ps: &SyntaxSet, theme: &Theme) -> io::Result<()> {
    // jj --tool / ui.diff-formatter invokes us with $left $right directories; don't page (jj pages).
    let use_pager = !cli.tool_mode() && !cli.no_pager && stdout_is_tty();

    let input: Box<dyn BufRead> = if cli.tool_mode() {
        Box::new(io::Cursor::new(unified_diff(&cli.paths[0], &cli.paths[1])?))
    } else {
        Box::new(io::stdin().lock())
    };

    if use_pager {
        run_with_pager(input, ps, theme, cli.underline)
    } else {
        process(input, &mut io::stdout().lock(), ps, theme, cli.underline)
    }
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
