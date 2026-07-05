use std::io::IsTerminal;
use serde::Deserialize;
use jsonrepair_rs::jsonrepair;

/// serde_json::Value that deserializes through serde_stacker, growing the
/// stack on demand so arbitrarily deep JSON parses instead of overflowing.
pub struct DeepValue(pub serde_json::Value);

impl<'de> Deserialize<'de> for DeepValue {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        serde_json::Value::deserialize(serde_stacker::Deserializer::new(d)).map(DeepValue)
    }
}

/// Parse-and-discard marker with the same stack-growing behaviour, for
/// validation passes that don't need to build the value.
struct DeepIgnored;

impl<'de> Deserialize<'de> for DeepIgnored {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        serde::de::IgnoredAny::deserialize(serde_stacker::Deserializer::new(d))?;
        Ok(DeepIgnored)
    }
}

/// Drop a Value iteratively; the compiler-generated recursive drop overflows
/// the stack on deeply nested values.
pub fn drop_deep(val: serde_json::Value) {
    let mut stack = vec![val];
    while let Some(v) = stack.pop() {
        match v {
            serde_json::Value::Array(arr) => stack.extend(arr),
            serde_json::Value::Object(map) => stack.extend(map.into_iter().map(|(_, v)| v)),
            _ => {}
        }
    }
}

fn parse_json(s: &str) -> Result<serde_json::Value, serde_json::Error> {
    let mut de = serde_json::Deserializer::from_str(s);
    de.disable_recursion_limit();
    let val = DeepValue::deserialize(&mut de)?.0;
    de.end()?;
    Ok(val)
}

/// Validate that the input is a stream of one or more whitespace-separated
/// JSON values. jq accepts these natively, so this covers single documents,
/// NDJSON, and multi-document input in one pass.
fn validate_stream(s: &str) -> Result<(), String> {
    let mut de = serde_json::Deserializer::from_str(s);
    de.disable_recursion_limit();
    let mut count = 0usize;
    for item in de.into_iter::<DeepIgnored>() {
        item.map_err(|e| e.to_string())?;
        count += 1;
    }
    if count == 0 {
        return Err("empty input".to_string());
    }
    Ok(())
}

/// Split input into its leading valid JSON values (verbatim text) plus the
/// unparseable remainder ("" when everything parsed). The bool reports
/// whether parsing stopped on a truncation (unexpected EOF) rather than
/// malformed content.
fn scan_values(s: &str) -> (Vec<String>, &str, bool) {
    let mut de = serde_json::Deserializer::from_str(s);
    de.disable_recursion_limit();
    let mut stream = de.into_iter::<DeepIgnored>();
    let mut values = Vec::new();
    let mut prev = 0usize;
    loop {
        match stream.next() {
            Some(Ok(_)) => {
                let end = stream.byte_offset();
                values.push(s[prev..end].trim().to_string());
                prev = end;
            }
            Some(Err(e)) => return (values, s[prev..].trim(), e.is_eof()),
            None => return (values, "", false),
        }
    }
}

fn warn(msg: &str, quiet: bool) {
    if quiet {
        return;
    }
    if std::io::stderr().is_terminal() {
        eprintln!("\x1b[33mjqr: {msg}\x1b[0m");
    } else {
        eprintln!("jqr: {msg}");
    }
}

/// Repair a source string. Returns a Vec of valid JSON stream strings.
/// Valid input (single doc, NDJSON, or multi-document stream) passes through
/// unchanged. A stream cut off inside its final value keeps the intact
/// leading values and repairs only the tail. Line-oriented streams with
/// broken records mid-stream are repaired per-line so the intact records
/// survive; everything else goes through whole-input repair.
pub fn repair_source(raw: &str, strict: bool, quiet: bool) -> Result<Vec<String>, String> {
    if strict {
        validate_stream(raw).map_err(|e| format!("invalid JSON: {e}"))?;
        return Ok(vec![raw.to_string()]);
    }

    // Fast path: already a valid JSON value stream
    if validate_stream(raw).is_ok() {
        return Ok(vec![raw.to_string()]);
    }

    // Stream cut off inside its final value (NDJSON or whitespace-separated
    // documents, on any number of lines): keep the intact leading values
    // verbatim and repair only the truncated tail.
    let (values, rest, truncated) = scan_values(raw);
    if truncated && !values.is_empty() && !rest.is_empty() {
        let repaired = jsonrepair(rest)
            .map_err(|e| format!("repair failed on truncated final record: {e}"))?;
        validate_stream(&repaired)
            .map_err(|e| format!("repair produced invalid JSON: {e}"))?;
        warn(
            "repaired truncated final record (it may be incomplete)",
            quiet,
        );
        let mut out = values;
        out.push(repaired);
        return Ok(out);
    }

    // Line-oriented input (NDJSON-ish) broken somewhere mid-stream: when at
    // least half the lines are complete JSON records, repair each broken line
    // on its own so the intact records survive. The threshold keeps broken
    // pretty-printed documents (whose lines are fragments) out of this path.
    let lines: Vec<&str> = raw.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if lines.len() >= 2 {
        let parseable = lines.iter().filter(|l| validate_stream(l).is_ok()).count();
        if parseable * 2 >= lines.len() {
            if let Some(out) = repair_lines(&lines, quiet) {
                return Ok(out);
            }
        }
    }

    // Whole-input repair
    let repaired = jsonrepair(raw).map_err(|e| format!("repair failed: {e}"))?;
    let val = parse_json(&repaired)
        .map_err(|e| format!("repair produced invalid JSON: {e}"))?;

    if val.is_string() && !raw.trim_start().starts_with('"') {
        warn(
            "input did not look like JSON; coerced to a JSON string",
            quiet,
        );
    } else {
        warn(
            "repaired broken JSON (data may be missing if the input was truncated)",
            quiet,
        );
    }
    drop_deep(val);
    Ok(vec![repaired])
}

/// Repair a line-oriented stream line by line, keeping each line's leading
/// valid values and repairing only the broken rest. Returns None if any
/// broken segment cannot be repaired (callers fall back to whole-input
/// repair).
fn repair_lines(lines: &[&str], quiet: bool) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    let mut repaired_count = 0usize;
    for line in lines {
        if validate_stream(line).is_ok() {
            out.push((*line).to_string());
            continue;
        }
        let (values, rest, _) = scan_values(line);
        out.extend(values);
        if rest.is_empty() {
            continue;
        }
        let repaired = jsonrepair(rest).ok()?;
        validate_stream(&repaired).ok()?;
        out.push(repaired);
        repaired_count += 1;
    }
    if repaired_count > 0 {
        warn(
            &format!(
                "repaired {repaired_count} broken record(s) mid-stream (non-JSON coerced, data may be missing)"
            ),
            quiet,
        );
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_single_doc_passes_through_unchanged() {
        let raw = r#"{"a":1,"b":[2,3]}"#;
        assert_eq!(repair_source(raw, false, true).unwrap(), vec![raw.to_string()]);
    }

    #[test]
    fn valid_ndjson_passes_through_unchanged() {
        let raw = "{\"id\":1}\n{\"id\":2}\n{\"id\":3}";
        assert_eq!(repair_source(raw, false, true).unwrap(), vec![raw.to_string()]);
    }

    #[test]
    fn valid_multidoc_stream_passes_through_unchanged() {
        let raw = r#"{"a":1} {"a":2}"#;
        assert_eq!(repair_source(raw, false, true).unwrap(), vec![raw.to_string()]);
    }

    #[test]
    fn truncated_single_doc_is_repaired() {
        let out = repair_source(r#"{"name":"vlad","items":[1,2,3"#, false, true).unwrap();
        assert_eq!(out.len(), 1);
        let v: serde_json::Value = serde_json::from_str(&out[0]).unwrap();
        assert_eq!(v["items"], serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn truncated_ndjson_tail_keeps_intact_records() {
        let raw = "{\"id\":1,\"v\":\"a\"}\n{\"id\":2,\"v\":\"b\"}\n{\"id\":3,\"v\":";
        let out = repair_source(raw, false, true).unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], "{\"id\":1,\"v\":\"a\"}");
        assert_eq!(out[1], "{\"id\":2,\"v\":\"b\"}");
        let v: serde_json::Value = serde_json::from_str(&out[2]).unwrap();
        assert_eq!(v["id"], 3);
    }

    #[test]
    fn truncated_multidoc_on_one_line_keeps_intact_values() {
        let out = repair_source(r#"{"a":1} {"a":2"#, false, true).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], r#"{"a":1}"#);
        let v: serde_json::Value = serde_json::from_str(&out[1]).unwrap();
        assert_eq!(v["a"], 2);
    }

    #[test]
    fn broken_middle_record_keeps_intact_records() {
        let raw = "{\"a\":1}\n{\"a\":2} garbage\n{\"a\":3}";
        let out = repair_source(raw, false, true).unwrap();
        assert_eq!(out.len(), 4);
        assert_eq!(out[0], "{\"a\":1}");
        assert_eq!(out[1], "{\"a\":2}");
        let coerced: serde_json::Value = serde_json::from_str(&out[2]).unwrap();
        assert!(coerced.is_string());
        assert_eq!(out[3], "{\"a\":3}");
    }

    #[test]
    fn broken_first_record_keeps_later_records() {
        let raw = "not json\n{\"a\":2}\n{\"a\":3}";
        let out = repair_source(raw, false, true).unwrap();
        assert_eq!(out.len(), 3);
        let coerced: serde_json::Value = serde_json::from_str(&out[0]).unwrap();
        assert!(coerced.is_string());
        assert_eq!(out[1], "{\"a\":2}");
        assert_eq!(out[2], "{\"a\":3}");
    }

    #[test]
    fn broken_pretty_printed_doc_avoids_line_repair() {
        // Lines are fragments, so this must go through whole-input repair
        // and come back as one document, not a pile of coerced strings.
        let raw = "{\n  \"a\": 1,\n  \"b\": [1, 2\n";
        let out = repair_source(raw, false, true).unwrap();
        assert_eq!(out.len(), 1);
        let v: serde_json::Value = serde_json::from_str(&out[0]).unwrap();
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"], serde_json::json!([1, 2]));
    }

    #[test]
    fn deeply_nested_valid_json_does_not_overflow() {
        let depth = 100_000;
        let raw = format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
        let out = repair_source(&raw, false, true).unwrap();
        assert_eq!(out, vec![raw]);
    }

    #[test]
    fn garbage_is_coerced_to_string_not_mangled() {
        let out = repair_source("Internal Server Error", false, true).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out[0]).unwrap();
        assert!(v.is_string());
    }

    #[test]
    fn strict_rejects_broken_json() {
        assert!(repair_source(r#"{"a":"#, true, true).is_err());
    }

    #[test]
    fn strict_accepts_valid_ndjson_and_multidoc() {
        assert!(repair_source("{\"a\":1}\n{\"a\":2}", true, true).is_ok());
        assert!(repair_source(r#"{"a":1} {"a":2}"#, true, true).is_ok());
    }
}
