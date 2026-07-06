use std::env;
use std::io::{self, IsTerminal, Read, Write};
use std::process::{Command, Stdio, exit};
use std::thread;

mod args;
mod repair;

fn main() {
    // jqr accepts arbitrarily deep JSON and jsonrepair recurses on nesting
    // depth, so run everything on a thread with a large stack. jqr's own
    // parsing and printing grow the stack on demand via serde_stacker.
    let worker = thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(run)
        .expect("jqr: failed to spawn worker thread");
    if worker.join().is_err() {
        exit(101);
    }
}

fn run() {
    let argv: Vec<String> = env::args().skip(1).collect();

    // No args on a TTY: show help
    if argv.is_empty() && io::stdin().is_terminal() {
        print_help();
        return;
    }

    // Extract jqr-specific flags, respecting argument positions.
    // Flags that are values to --arg/--argjson/etc must not be stripped.
    let (no_jq, quiet, strict, show_help, show_version, argv) = extract_jqr_flags(&argv);

    if show_help {
        print_help();
        return;
    }

    if show_version {
        print_version();
        return;
    }

    // Meta flags: pass straight to jq
    if argv.iter().any(|a| args::is_meta_flag(a)) {
        let status = Command::new("jq")
            .args(&argv)
            .status()
            .unwrap_or_else(|_| { eprintln!("jqr: jq not found"); exit(1); });
        exit(status.code().unwrap_or(1));
    }

    let parsed = args::parse_jq_argv(&argv, !no_jq);

    // -R or -n: pass through to jq without repair
    if parsed.is_raw_input || parsed.is_null_input {
        if no_jq {
            eprintln!("jqr: --no-jq has no effect with -R or -n");
        }
        let mut jq_args = parsed.jq_argv.clone();
        jq_args.extend(parsed.input_files.clone());
        let status = Command::new("jq")
            .args(&jq_args)
            .status()
            .unwrap_or_else(|_| { eprintln!("jqr: jq not found"); exit(1); });
        exit(status.code().unwrap_or(1));
    }

    // Read and repair sources
    let sources = match read_sources(&parsed.input_files) {
        Ok(s) => s,
        Err(e) => { eprintln!("jqr: {e}"); exit(1); }
    };

    if sources.iter().all(|(_, raw)| raw.trim().is_empty()) {
        eprintln!("jqr: empty input");
        exit(1);
    }

    let mut objects: Vec<String> = Vec::new();
    for (label, raw) in &sources {
        if raw.trim().is_empty() {
            eprintln!("jqr: skipping empty input: {label}");
            continue;
        }
        match repair::repair_source(raw, strict, quiet) {
            Ok(repaired) => objects.extend(repaired),
            Err(e) => { eprintln!("jqr: {e}"); exit(1); }
        }
    }

    if no_jq {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let compact = parsed.jq_argv.iter().any(|a| a == "-c" || a == "--compact-output");
        let use_tab = parsed.jq_argv.iter().any(|a| a == "--tab");
        let spaces = " ".repeat(get_indent(&parsed.jq_argv));
        // Each element is a validated JSON value stream (possibly multiple
        // whitespace-separated values); print every value in it.
        for json_str in &objects {
            let mut de = serde_json::Deserializer::from_str(json_str);
            de.disable_recursion_limit();
            for item in de.into_iter::<repair::DeepValue>() {
                let Ok(repair::DeepValue(val)) = item else { break };
                let formatted = if compact {
                    serialize_deep(&val, serde_json::ser::CompactFormatter)
                } else if use_tab {
                    serialize_deep(&val, serde_json::ser::PrettyFormatter::with_indent(b"\t"))
                } else {
                    serialize_deep(&val, serde_json::ser::PrettyFormatter::with_indent(spaces.as_bytes()))
                };
                let _ = writeln!(out, "{formatted}");
                repair::drop_deep(val);
            }
        }
        return;
    }

    // Feed the repaired stream to jq. Its stdout/stderr are inherited, so
    // output streams through unbuffered, per-input errors reach stderr even
    // when jq exits 0, and no deadlock is possible: we hold only jq's stdin.
    let ndjson = objects.join("\n");

    let mut child = Command::new("jq")
        .args(&parsed.jq_argv)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap_or_else(|_| { eprintln!("jqr: jq not found"); exit(1); });

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(ndjson.as_bytes());
    }

    let status = child.wait().unwrap_or_else(|e| {
        eprintln!("jqr: failed to run jq: {e}");
        exit(1);
    });
    exit(status.code().unwrap_or(1));
}

/// Extract jqr-specific flags from argv, skipping tokens that are consumed as
/// values by preceding options (--arg, --argjson, etc).
fn extract_jqr_flags(argv: &[String]) -> (bool, bool, bool, bool, bool, Vec<String>) {
    let mut no_jq = false;
    let mut quiet = false;
    let mut strict = false;
    let mut show_help = false;
    let mut show_version = false;
    let mut result: Vec<String> = Vec::new();

    // jq keeps recognising options after --args/--jsonargs (only non-option
    // tokens become positionals), so -V/--version/-h/--help stay intercepted
    // there, but jqr-only flags are passed through so jq can reject them the
    // same way it would reject them itself. Only -- ends option parsing.
    let mut args_mode = false;

    let mut i = 0;
    while i < argv.len() {
        let tok = &argv[i];

        // After the option terminator, everything is filter/file arguments
        // for jq, never jqr flags
        if tok == "--" {
            result.extend(argv[i..].iter().cloned());
            break;
        }

        if tok == "--args" || tok == "--jsonargs" {
            args_mode = true;
            result.push(tok.clone());
            i += 1;
            continue;
        }

        // Check if this is a long option that consumes extra tokens
        if tok.starts_with("--") && !tok.contains('=') {
            let extra = args::long_option_extra(tok);
            if extra > 0 {
                // Push this flag and its values through unchanged
                result.push(tok.clone());
                for j in 1..=extra {
                    if i + j < argv.len() {
                        result.push(argv[i + j].clone());
                    }
                }
                i += 1 + extra;
                continue;
            }
        }

        // Check if this is a short flag bundle containing an extra-consuming flag
        if tok.starts_with('-') && !tok.starts_with("--") && tok != "-" {
            let chars: Vec<char> = tok[1..].chars().collect();
            let has_extra = chars.iter().any(|c| args::short_flag_extra(*c) > 0);
            if has_extra {
                // This bundle consumes a following token; pass through unchanged
                result.push(tok.clone());
                // Find the extra-consuming char and count tokens it needs
                for (ci, &ch) in chars.iter().enumerate() {
                    let extra = args::short_flag_extra(ch);
                    if extra > 0 {
                        let mut remaining = extra;
                        if ci + 1 < chars.len() {
                            remaining -= 1; // rest of bundle is first arg
                        }
                        for _ in 0..remaining {
                            i += 1;
                            if i < argv.len() {
                                result.push(argv[i].clone());
                            }
                        }
                        break;
                    }
                }
                i += 1;
                continue;
            }

            // Simple bundle (no extra-consuming flags): intercept bundled
            // V/h the way jq itself would, pass the rest through
            if tok.len() > 2 && chars.iter().any(|c| *c == 'V' || *c == 'h') {
                if chars.contains(&'V') {
                    show_version = true;
                }
                if chars.contains(&'h') {
                    show_help = true;
                }
                let rest: String = chars.iter().filter(|c| **c != 'V' && **c != 'h').collect();
                if !rest.is_empty() {
                    result.push(format!("-{rest}"));
                }
                i += 1;
                continue;
            }
        }

        match tok.as_str() {
            "--no-jq" if !args_mode => no_jq = true,
            "--quiet" if !args_mode => quiet = true,
            "--strict" if !args_mode => strict = true,
            "-h" | "--help" => show_help = true,
            "-V" | "--version" => show_version = true,
            _ => result.push(tok.clone()),
        }
        i += 1;
    }

    (no_jq, quiet, strict, show_help, show_version, result)
}

fn print_version() {
    let jqr_version = env!("CARGO_PKG_VERSION");
    match Command::new("jq").arg("--version").output() {
        Ok(o) if o.status.success() => {
            let jq_version = String::from_utf8_lossy(&o.stdout).trim().to_string();
            println!("jqr {jqr_version} ({jq_version})");
        }
        _ => println!("jqr {jqr_version}"),
    }
}

fn print_help() {
    println!(
        "jqr - jq for broken JSON.

Repairs incomplete/mangled JSON (truncated responses, corrupted logs,
streaming JSON cut off mid-flight), then applies a jq filter to the result.
Warnings about what was repaired go to stderr; clean output goes to stdout.

Usage:
  jqr [JQR-FLAGS] [JQ-OPTIONS] [FILTER] [FILE...]
  <stdin> | jqr [JQR-FLAGS] [JQ-OPTIONS] [FILTER]

jqr-specific flags (not passed to jq):
  --no-jq        Repair and pretty-print only, do not run jq
  --quiet        Suppress repair warnings
  --strict       Fail if input is not valid JSON (no repair)
  -V, --version  Show jqr and jq versions
  -h, --help     Show this help

All other options and flags are passed to jq unchanged.
See jq --help for the full list.

Examples:
  curl -s api/endpoint | jqr '.data[]'
  jqr -r '.name' broken.json
  jqr -s '.[].id' a.json b.json
  jqr --no-jq truncated.json
  jqr --quiet -c '.items[]' partial.json"
    );
}

fn read_sources(files: &[String]) -> io::Result<Vec<(String, String)>> {
    if files.is_empty() {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf)?;
        return Ok(vec![("<stdin>".to_string(), buf)]);
    }
    let mut sources = Vec::new();
    for f in files {
        if f == "-" {
            let mut buf = String::new();
            io::stdin().read_to_string(&mut buf)?;
            sources.push(("<stdin>".to_string(), buf));
        } else {
            let content = std::fs::read_to_string(f)
                .map_err(|e| io::Error::new(e.kind(), format!("{f}: {e}")))?;
            sources.push((f.clone(), content));
        }
    }
    Ok(sources)
}

fn get_indent(jq_argv: &[String]) -> usize {
    let mut indent = 2usize;
    let mut i = 0;
    while i < jq_argv.len() {
        if jq_argv[i] == "--indent" {
            if let Some(val) = jq_argv.get(i + 1) {
                if let Ok(n) = val.parse::<usize>() {
                    indent = n.min(7);
                }
            }
        } else if let Some(val) = jq_argv[i].strip_prefix("--indent=") {
            if let Ok(n) = val.parse::<usize>() {
                indent = n.min(7);
            }
        }
        i += 1;
    }
    indent
}

/// Serialize through serde_stacker so deeply nested values grow the stack
/// on demand instead of overflowing it.
fn serialize_deep<F: serde_json::ser::Formatter>(val: &serde_json::Value, formatter: F) -> String {
    use serde::Serialize;
    let mut buf = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
    let _ = val.serialize(serde_stacker::Serializer::new(&mut ser));
    String::from_utf8(buf).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sv(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn dashdash_stops_jqr_flag_stripping() {
        let (no_jq, quiet, strict, help, version, rest) =
            extract_jqr_flags(&sv(&["-n", "--", "--quiet", "--no-jq", "--help", "--version"]));
        assert!(!no_jq && !quiet && !strict && !help && !version);
        assert_eq!(rest, sv(&["-n", "--", "--quiet", "--no-jq", "--help", "--version"]));
    }

    #[test]
    fn jqr_flags_before_dashdash_still_stripped() {
        let (_, quiet, _, _, _, rest) = extract_jqr_flags(&sv(&["--quiet", "--", "--no-jq"]));
        assert!(quiet);
        assert_eq!(rest, sv(&["--", "--no-jq"]));
    }

    #[test]
    fn version_flag_is_intercepted() {
        let (_, _, _, _, version, rest) = extract_jqr_flags(&sv(&["--version"]));
        assert!(version);
        assert!(rest.is_empty());
        let (_, _, _, _, version, _) = extract_jqr_flags(&sv(&["-V"]));
        assert!(version);
    }

    #[test]
    fn bundled_version_flag_is_intercepted() {
        let (_, _, _, _, version, rest) = extract_jqr_flags(&sv(&["-nV"]));
        assert!(version);
        assert_eq!(rest, sv(&["-n"]));
        let (_, _, _, help, version, rest) = extract_jqr_flags(&sv(&["-Vh"]));
        assert!(version && help);
        assert!(rest.is_empty());
    }

    #[test]
    fn version_after_args_mode_is_still_intercepted() {
        let (_, _, _, _, version, rest) =
            extract_jqr_flags(&sv(&["--args", "$ARGS.positional", "--version"]));
        assert!(version);
        assert_eq!(rest, sv(&["--args", "$ARGS.positional"]));
    }

    #[test]
    fn jqr_flags_after_args_mode_pass_through_to_jq() {
        let (_, quiet, _, _, _, rest) = extract_jqr_flags(&sv(&["--args", ".", "--quiet"]));
        assert!(!quiet);
        assert_eq!(rest, sv(&["--args", ".", "--quiet"]));
    }

    #[test]
    fn version_as_arg_value_is_not_intercepted() {
        let (_, _, _, _, version, rest) =
            extract_jqr_flags(&sv(&["--arg", "x", "--version", "."]));
        assert!(!version);
        assert_eq!(rest, sv(&["--arg", "x", "--version", "."]));
    }
}
