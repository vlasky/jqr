/// jq short flags that consume the next token as an argument.
const SHORT_EXTRA: &[(char, usize)] = &[('f', 1), ('L', 1)];

/// jq long options that consume extra tokens after themselves.
const LONG_EXTRA: &[(&str, usize)] = &[
    ("--indent", 1),
    ("--from-file", 1),
    ("--library-path", 1),
    ("--arg", 2),
    ("--argjson", 2),
    ("--slurpfile", 2),
    ("--rawfile", 2),
];

const META_FLAGS: &[&str] = &["-V", "--version", "--build-configuration"];

pub fn is_meta_flag(s: &str) -> bool {
    META_FLAGS.contains(&s)
}

pub struct ParsedArgs {
    pub input_files: Vec<String>,
    pub jq_argv: Vec<String>,
    pub is_null_input: bool,
    pub is_raw_input: bool,
}

fn short_extra(ch: char) -> usize {
    SHORT_EXTRA.iter().find(|(c, _)| *c == ch).map_or(0, |(_, n)| *n)
}

fn long_extra(s: &str) -> usize {
    // Strip =value suffix for lookup
    let key = s.split('=').next().unwrap_or(s);
    LONG_EXTRA.iter().find(|(k, _)| *k == key).map_or(0, |(_, n)| *n)
}

pub fn parse_jq_argv(argv: &[String], need_filter: bool) -> ParsedArgs {
    let mut input_files: Vec<String> = Vec::new();
    let mut jq_argv: Vec<String> = Vec::new();
    let mut is_null_input = false;
    let mut is_raw_input = false;
    let mut has_from_file = false;
    let mut past_dashdash = false;
    let mut args_mode = false;
    let mut have_filter = false;

    let mut i = 0;
    while i < argv.len() {
        let tok = &argv[i];

        if args_mode {
            jq_argv.push(tok.clone());
            i += 1;
            continue;
        }

        if tok == "--args" || tok == "--jsonargs" {
            args_mode = true;
            jq_argv.push(tok.clone());
            i += 1;
            continue;
        }

        if tok == "--" {
            past_dashdash = true;
            jq_argv.push(tok.clone());
            i += 1;
            continue;
        }

        if !past_dashdash && tok.starts_with('-') && tok != "-" {
            if tok.starts_with("--") {
                // Long option
                match tok.as_str() {
                    "--null-input" => is_null_input = true,
                    "--raw-input" => is_raw_input = true,
                    "--from-file" => has_from_file = true,
                    _ => {}
                }

                jq_argv.push(tok.clone());

                if tok.contains('=') {
                    i += 1;
                    continue;
                }

                let extra = long_extra(tok);
                i += 1;
                for _ in 0..extra {
                    if i < argv.len() {
                        jq_argv.push(argv[i].clone());
                        i += 1;
                    }
                }
                continue;
            }

            // Short flags: decompose bundled flags (-Rn -> -R -n)
            let chars: Vec<char> = tok[1..].chars().collect();
            let mut ci = 0;
            while ci < chars.len() {
                let ch = chars[ci];
                match ch {
                    'n' => is_null_input = true,
                    'R' => is_raw_input = true,
                    'f' => has_from_file = true,
                    _ => {}
                }
                jq_argv.push(format!("-{ch}"));

                let extra = short_extra(ch);
                ci += 1;
                if extra > 0 {
                    // Remaining bundled chars become the first extra arg
                    let mut remaining_extra = extra;
                    if ci < chars.len() {
                        let rest: String = chars[ci..].iter().collect();
                        jq_argv.push(rest);
                        remaining_extra -= 1;
                    }
                    // Consume further tokens from argv
                    for _ in 0..remaining_extra {
                        i += 1;
                        if i < argv.len() {
                            jq_argv.push(argv[i].clone());
                        }
                    }
                    break;
                }
            }
            i += 1;
            continue;
        }

        // Positional argument
        if need_filter && !has_from_file && !have_filter {
            have_filter = true;
            jq_argv.push(tok.clone());
        } else {
            input_files.push(tok.clone());
        }
        i += 1;
    }

    ParsedArgs {
        input_files,
        jq_argv,
        is_null_input,
        is_raw_input,
    }
}
