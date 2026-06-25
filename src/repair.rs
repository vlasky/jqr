use std::io::IsTerminal;
use serde::Deserialize;
use jsonrepair_rs::jsonrepair;

fn parse_json(s: &str) -> Result<serde_json::Value, serde_json::Error> {
    let mut de = serde_json::Deserializer::from_str(s);
    de.disable_recursion_limit();
    serde_json::Value::deserialize(&mut de)
}

/// Detect if input is newline-delimited JSON (every non-empty line is valid JSON).
fn is_ndjson(raw: &str) -> bool {
    let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < 2 {
        return false;
    }
    lines.iter().all(|line| parse_json(line).is_ok())
}

/// Repair a source string. Returns a Vec of valid JSON strings (one per value).
/// Detects NDJSON and repairs each line independently.
pub fn repair_source(raw: &str, strict: bool, quiet: bool) -> Result<Vec<String>, String> {
    if is_ndjson(raw) {
        let mut results = Vec::new();
        for line in raw.lines() {
            if line.trim().is_empty() {
                continue;
            }
            results.push(repair_single(line, strict, quiet)?);
        }
        return Ok(results);
    }
    Ok(vec![repair_single(raw, strict, quiet)?])
}

fn repair_single(raw: &str, strict: bool, quiet: bool) -> Result<String, String> {
    if strict {
        parse_json(raw).map_err(|e| format!("invalid JSON: {e}"))?;
        return Ok(raw.to_string());
    }

    // Fast path: already valid JSON
    if parse_json(raw).is_ok() {
        return Ok(raw.to_string());
    }

    if !quiet {
        if std::io::stderr().is_terminal() {
            eprintln!("\x1b[33mjqr: repaired broken JSON\x1b[0m");
        } else {
            eprintln!("jqr: repaired broken JSON");
        }
    }

    let repaired = jsonrepair(raw)
        .map_err(|e| format!("repair failed: {e}"))?;

    // Validate the repair produced valid JSON
    parse_json(&repaired)
        .map_err(|e| format!("repair produced invalid JSON: {e}"))?;

    Ok(repaired)
}
