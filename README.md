# lf

`lf` is a high-performance command-line tool that converts line endings of
text files to **LF**, recursively. It is built for the use case
"normalize an entire tree of source files to LF in one shot, safely and
quickly".

## Features

- **Recursive directory traversal** via the [`ignore`] crate, which honours
  `.gitignore` / `.ignore` / `.git/info/exclude` / global gitignore by default.
- **Multi-threaded**: file detection and conversion run on `ignore`'s built-in
  worker pool — all CPU cores are used automatically.
- **Smarter binary detection**: a combination of BOM recognition, NUL-byte
  scanning, and control-byte ratio on the first 8 KiB keeps false positives
  low (e.g. UTF-16 text is no longer misclassified as binary).
- **Atomic writes**: every rewrite goes through a temp file + `rename`, so an
  interrupted run never leaves a half-written file. On Unix the original
  file's permission bits (including the executable bit) are preserved.
- **Byte-level**: never assumes UTF-8 — Latin-1, Shift-JIS, etc. are processed
  just fine.
- The `.git` directory is **always** skipped, even under `--no-gitignore`.

## Installation

Pre-built binaries for Linux (glibc / musl, x86_64 / aarch64), macOS
(x86_64 / aarch64) and Windows (x86_64) are published on the
[Releases](https://github.com/lxl66566/lf/releases) page.

Use [cargo-binstall](https://github.com/cargo-bins/cargo-binstall):

```bash
cargo binstall lf --git https://github.com/lxl66566/lf
```

## Usage

```sh
lf [OPTIONS] [PATH]
```

If `PATH` is omitted, the current directory is used. `PATH` may be a single
file or a directory.

### Options

| Option            | Description                                                          |
| ----------------- | -------------------------------------------------------------------- |
| `-n, --dry-run`   | Print what _would_ be converted; do not write to disk.               |
| `-q, --quiet`     | Suppress per-file output. Summary on stderr is still printed.        |
| `-v, --verbose`   | Also print every file that was skipped (binary / already LF / ext).  |
| `--no-gitignore`  | Disable `.gitignore` / `.ignore` / `.git/info/exclude` / global git. |
| `--no-hidden`     | Skip hidden files and directories (Unix dotfiles).                   |
| `--max-depth N`   | Maximum recursion depth (`1` = direct children only).                |
| `-E, --ext a,b,c` | Comma-separated allow-list of extensions. Others are skipped.        |
| `--help`          | Show help.                                                           |

`--quiet` and `--verbose` conflict with each other.

### Examples

Convert every text file under the current directory:

```bash
lf
```

Preview only:

```bash
lf -n .
```

Convert all Rust and Markdown files in `src/`, ignoring the project's
`.gitignore`:

```bash
lf --no-gitignore --ext rs,md src
```

### Exit codes

| Code | Meaning                                           |
| ---- | ------------------------------------------------- |
| 0    | Success (files may or may not have been changed). |
| 1    | One or more files produced an I/O error.          |
| 2    | The given path does not exist.                    |
