use std::io::IsTerminal;
use serde::Deserialize;
use jsonrepair_rs::jsonrepair;

fn parse_json(s: &str) -> Result<serde_json::Value, serde_json::Error> {
    let mut de = serde_json::Deserializer::from_str(s);
    de.disable_recursion_limit();
    let val = serde_json::Value::deserialize(&mut de)?;
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
    for item in de.into_iter::<serde_json::Value>() {
        item.map_err(|e| e.to_string())?;
        count += 1;
    }
    if count == 0 {
        return Err("empty input".to_string());
    }
    Ok(())
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
/// unchanged. NDJSON with a broken final record is repaired per-line so the
/// intact records survive; everything else goes through whole-input repair.
pub fn repair_source(raw: &str, strict: bool, quiet: bool) -> Result<Vec<String>, String> {
    if strict {
        validate_stream(raw).map_err(|e| format!("invalid JSON: {e}"))?;
        return Ok(vec![raw.to_string()]);
    }

    // Fast path: already a valid JSON value stream
    if validate_stream(raw).is_ok() {
        return Ok(vec![raw.to_string()]);
    }

    // NDJSON cut off mid-record: every line but the last parses. Repair only
    // the final line so the intact records pass through untouched.
    let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() >= 2 {
        let (last, head) = lines.split_last().unwrap();
        if head.iter().all(|l| parse_json(l).is_ok()) {
            let repaired = jsonrepair(last)
                .map_err(|e| format!("repair failed on final NDJSON record: {e}"))?;
            parse_json(&repaired)
                .map_err(|e| format!("repair produced invalid JSON: {e}"))?;
            warn(
                "repaired truncated final NDJSON record (it may be incomplete)",
                quiet,
            );
            let mut out: Vec<String> = head.iter().map(|s| s.to_string()).collect();
            out.push(repaired);
            return Ok(out);
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
    Ok(vec![repaired])
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
