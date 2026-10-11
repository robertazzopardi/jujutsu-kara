# jj-diff-syntax-highlight

Syntax-highlight **code** in Jujutsu diffs. Headers / line numbers / hunk metadata pass through.

## Install

```bash
cargo install --path .
```

## Use with jj (color-words — recommended)

`--tool` / `ui.diff-formatter = <this binary>` **replaces** jj’s formatter, so you get unified `diff -ruN`, not color-words.

To keep default color-words and only add highlighting, use this as the **pager**:

```bash
jj config unset --user ui.diff-formatter   # if you pointed it at this tool
# leave formatter as :color-words (jj default)

jj config set --user ui.pager jj-diff-syntax-highlight
```

One-shot:

```bash
jj diff --color=always | jj-diff-syntax-highlight --no-pager
```

## Unified / `--tool` (optional)

```bash
jj diff --tool jj-diff-syntax-highlight   # unified only
```

## Options

| Flag / env | Meaning |
|---|---|
| `--theme NAME` | syntect theme (default `base16-ocean.dark`) |
| `--no-pager` | write to stdout (no `less`) |
| `$JJ_DIFF_THEME` | default theme override |
