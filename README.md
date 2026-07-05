# jqr - jq with JSON repair

[![CI](https://github.com/vlasky/jqr/actions/workflows/ci.yml/badge.svg)](https://github.com/vlasky/jqr/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/vlasky/jqr)](https://github.com/vlasky/jqr/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust 2024](https://img.shields.io/badge/rust-2024_edition-orange.svg)](https://www.rust-lang.org)

`jqr` ("jq repair") is a drop-in wrapper around [jq](https://jqlang.github.io/jq/) that repairs incomplete or mangled JSON before filtering it. Truncated API responses, corrupted log lines, streams cut off mid-flight: `jqr` salvages what is there, applies your jq filter to it, and tells you on stderr what it had to fix.

```console
$ printf '{"name":"vlad","items":[1,2,3' | jqr '.items | length'
jqr: repaired broken JSON (data may be missing if the input was truncated)
3
```

## Why

`jqr` was built for agentic coding tools (Claude Code, Codex, and friends). Agents put `jq` at the end of command pipelines constantly, and those pipelines routinely produce incomplete JSON: output capped with `head -c`, processes killed by timeouts, tool-result limits truncating a response mid-array. Plain `jq` fails on such input, so the agent re-runs the whole pipeline (or worse, dumps the raw JSON into context) and burns tokens either way.

`jqr` lets the agent use the partial output it already has. Intact records are preserved exactly; only the broken part is repaired. Warnings go to stderr so the agent knows the data may be incomplete, while stdout stays clean for the next stage of the pipeline.

## How it behaves

- **Valid input passes through untouched** - single documents, NDJSON, and whitespace-separated multi-document streams, exactly as `jq` would accept them.
- **Truncated final record**: the intact leading values are kept verbatim and only the cut-off tail is repaired, whether the stream is newline- or space-separated.
- **Broken record mid-stream** (line-oriented input): each broken line is repaired on its own, so the intact records around it survive. Non-JSON fragments are coerced to JSON strings.
- **Anything else** goes through whole-input repair (closing unterminated strings, brackets, and braces, quoting bare keys, and so on).
- **Arbitrarily deep nesting** is handled without a stack overflow.
- Everything else - options, filters, files, exit codes, output streaming - matches `jq`, because `jq` does the filtering.

## Install

### Homebrew

```sh
brew install vlasky/tap/jqr
```

### Cargo

Requires `jq` on your `PATH` and a Rust toolchain to build.

```sh
cargo install --git https://github.com/vlasky/jqr
```

Or from a clone:

```sh
cargo build --release
install target/release/jqr ~/.local/bin/   # or anywhere on PATH
```

## Usage

```
jqr [JQR-FLAGS] [JQ-OPTIONS] [FILTER] [FILE...]
<stdin> | jqr [JQR-FLAGS] [JQ-OPTIONS] [FILTER]
```

Flags specific to `jqr` (everything else is passed to `jq` unchanged):

| Flag | Effect |
|---|---|
| `--no-jq` | Repair and pretty-print only; do not run `jq` |
| `--quiet` | Suppress repair warnings |
| `--strict` | Fail if the input is not valid JSON (no repair) |

### Examples

A response cut off mid-flight still yields the intact records:

```console
$ printf '{"id":1}\n{"id":2}\n{"id":3,"v":' | jqr '.id'
jqr: repaired truncated final record (it may be incomplete)
1
2
3
```

A corrupted line in an NDJSON log does not take down the rest:

```console
$ printf '{"a":1}\n{"a":2} garbage\n{"a":3}' | jqr -c '.'
jqr: repaired 1 broken record(s) mid-stream (non-JSON coerced, data may be missing)
{"a":1}
{"a":2}
"garbage"
{"a":3}
```

Just fix the JSON, no filtering:

```console
$ curl -s api/endpoint | jqr --no-jq
```

Gate on validity when repair would be the wrong call:

```console
$ jqr --strict '.total' report.json
```

## License

[MIT](LICENSE) © Vlad Lasky
