//! Command-line argument parsing.

use std::env;

const USAGE: &str = "usage: kara [options] [LEFT RIGHT]";

/// Parsed command-line options.
pub struct Args {
    /// Theme name: `--theme`, else `$JJ_DIFF_THEME`, else GitHub Dark Default.
    pub theme: String,
    /// `--no-pager`: never spawn `less`.
    pub no_pager: bool,
    /// `--underline`: re-apply jj word-diff underlines.
    pub underline: bool,
    /// Positional paths: empty, or `LEFT RIGHT` (enforced by [`parse`]).
    pub paths: Vec<String>,
}

impl Args {
    /// jj --tool mode: invoked as `kara LEFT RIGHT`.
    pub fn tool_mode(&self) -> bool {
        self.paths.len() == 2
    }
}

/// Parse `args` (the program arguments, without argv[0]).
///
/// Prints help / version and exits 0 on `-h` / `-V`, and exits 2 on a usage error.
pub fn parse(args: impl IntoIterator<Item = String>) -> Args {
    let mut theme = env::var("JJ_DIFF_THEME").unwrap_or_else(|_| "GitHub Dark Default".into());
    let mut no_pager = false;
    let mut underline = false;
    let mut paths: Vec<String> = Vec::new();
    let mut positional_only = false;

    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if positional_only {
            paths.push(arg);
        } else if arg == "--" {
            positional_only = true;
        } else if arg == "-h" || arg == "--help" {
            print_help();
            std::process::exit(0);
        } else if arg == "-V" || arg == "--version" {
            println!("kara {}", env!("CARGO_PKG_VERSION"));
            std::process::exit(0);
        } else if let Some(value) = arg.strip_prefix("--theme=") {
            theme = value.to_string();
        } else if arg == "--theme" {
            theme = args
                .next()
                .unwrap_or_else(|| usage_error("--theme requires a value"));
        } else if arg == "--no-pager" {
            no_pager = true;
        } else if arg == "--underline" {
            underline = true;
        } else if arg.starts_with('-') {
            usage_error(&format!("unknown argument: {arg}"));
        } else {
            paths.push(arg);
        }
    }

    match paths.len() {
        0 | 2 => {}
        _ => usage_error("expected either no paths, or both LEFT and RIGHT"),
    }

    Args {
        theme,
        no_pager,
        underline,
        paths,
    }
}

fn usage_error(msg: &str) -> ! {
    eprintln!("kara: {msg}");
    eprintln!("{USAGE}");
    std::process::exit(2);
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
                 (also accepts --theme=NAME)
  --no-pager     never spawn less (default when stdout is not a TTY, or tool mode)
  --underline    re-apply jj word-diff underlines (off by default)
  --             treat the rest of the arguments as paths
  -h, --help     show this help
  -V, --version  print version
"
    );
}
