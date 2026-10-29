//! Empty input behaves exactly as it does under jq: same stdout, same exit code.

use std::io::Write;
use std::process::{Command, Output, Stdio};

fn run(program: &str, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to run {program}: {e}"));
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

fn assert_matches_jq(args: &[&str], stdin: &str) {
    let jqr = run(env!("CARGO_BIN_EXE_jqr"), args, stdin);
    let jq = run("jq", args, stdin);
    assert_eq!(jqr.status.code(), jq.status.code(), "exit code for {args:?} on {stdin:?}");
    assert_eq!(jqr.stdout, jq.stdout, "stdout for {args:?} on {stdin:?}");
    assert!(jqr.stderr.is_empty(), "jqr stderr for {args:?}: {}", String::from_utf8_lossy(&jqr.stderr));
}

#[test]
fn empty_stdin_matches_jq() {
    for args in [&["."][..], &["-r", ".id"], &["-e", "."], &["-s", "."], &["-c", "-s", "length"]] {
        assert_matches_jq(args, "");
        assert_matches_jq(args, "  \n\n");
    }
}

#[test]
fn empty_input_with_no_jq_prints_nothing() {
    let out = run(env!("CARGO_BIN_EXE_jqr"), &["--no-jq"], "");
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty());
}
