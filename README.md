# jujutsu-kara

Syntax-highlight **code** in Jujutsu diffs. Headers / line numbers / hunk metadata pass through.

Binary: `kara`.

## Install

```bash
cargo install --git https://github.com/robertazzopardi/jujutsu-kara
# or from a local clone:
cargo install --path .
```

Requires Rust 1.85+ (edition 2024).

## Use with jj (color-words — recommended)

`--tool` / `ui.diff-formatter = <this binary>` **replaces** jj’s formatter, so you get unified `diff -ruN`, not color-words.

To keep default color-words and only add highlighting, use this as the **pager**:

```bash
jj config unset --user ui.diff-formatter   # if you pointed it at this tool
# leave formatter as :color-words (jj default)

jj config set --user ui.pager kara
# Colored color-words collapses line rewrites into after-only dual lines (before
# never reaches the pager). Kara paints add/del washes itself — keep jj uncolored:
jj config set --user ui.color never
```

If you want jj’s own colors instead, keep `ui.color = "auto"` and force full
before/after lines:

```bash
jj config set --user diff.color-words.max-inline-alternation 0
```

One-shot:

```bash
jj diff --color=never | kara --no-pager
```

## Unified / `--tool` (optional)

```bash
jj diff --tool kara   # unified only
```

## Options

| Flag / env | Meaning |
|---|---|
| `--theme NAME` | default **GitHub Dark Default** (embedded Primer theme); other [two-face](https://crates.io/crates/two-face) names also work |
| `--no-pager` | write to stdout (no `less`) |
| `--underline` | re-apply jj word-diff underlines |
| `$JJ_DIFF_THEME` | default theme override |

Added/removed lines get a dark green/red background with syntax colors on top (closer to IDE diffs).

## License

MIT
