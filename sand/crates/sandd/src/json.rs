//! JSON helpers for sandd's hand-rolled JSON layer.
//!
//! sandd deliberately avoids serde in its RPC path: requests are small, the
//! shapes are fixed, and hand parsing keeps the daemon dependency-free. These
//! helpers are the single implementation shared by `rpc`, `rpc_binary`, `fs`
//! and `process`.

/// Escape a string for embedding inside a JSON string literal.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// `"field":"value"` → `value` (tolerates spaces and unescapes).
pub fn get_str(json: &str, field: &str) -> Option<String> {
    let patterns = [
        format!("\"{}\":\"", field),
        format!("\"{}\": \"", field),
        format!("\"{}\" : \"", field),
    ];
    for pat in &patterns {
        if let Some(start) = json.find(pat.as_str()) {
            let rest = &json[start + pat.len()..];
            if let Some(end) = rest.find('"') {
                return Some(unescape(&rest[..end]));
            }
        }
    }
    None
}

/// `"field":123` → `123`.
pub fn get_u64(json: &str, field: &str) -> Option<u64> {
    let pat = format!("\"{}\":", field);
    let start = json.find(pat.as_str())?;
    let rest = json[start + pat.len()..].trim_start();
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    rest[..end].parse().ok()
}

/// `"field":true` → `true`.
pub fn get_bool(json: &str, field: &str) -> Option<bool> {
    let pat = format!("\"{}\":", field);
    let start = json.find(pat.as_str())?;
    let rest = json[start + pat.len()..].trim_start();
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

/// `"field":["a","b"]` → `["a","b"]`.
pub fn get_str_array(json: &str, field: &str) -> Vec<String> {
    let pat = format!("\"{}\":[", field);
    let alt = format!("\"{}\": [", field);
    let start = match json.find(pat.as_str()) {
        Some(pos) => pos + pat.len(),
        None => match json.find(alt.as_str()) {
            Some(pos) => pos + alt.len(),
            None => return Vec::new(),
        },
    };
    let rest = &json[start..];
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    let mut escaped = false;
    for c in rest.chars() {
        if in_string {
            if escaped {
                current.push(c);
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
                out.push(current.trim().to_string());
                current.clear();
            } else {
                current.push(c);
            }
            continue;
        }
        match c {
            ']' => break,
            '"' => in_string = true,
            _ => {}
        }
    }
    out.into_iter().filter(|s| !s.is_empty()).collect()
}

/// True when the response is marked `"ok":true`.
pub fn is_ok(json: &str) -> bool {
    get_bool(json, "ok").unwrap_or(false)
}

/// `"error":"..."` when present.
pub fn error_of(json: &str) -> Option<String> {
    get_str(json, "error")
}

/// Unescape the subset of JSON escapes we ever emit.
pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('/') => out.push('/'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                if let Ok(code) = u32::from_str_radix(&hex, 16) {
                    if let Some(ch) = char::from_u32(code) {
                        out.push(ch);
                    }
                }
            }
            Some(other) => out.push(other),
            None => break,
        }
    }
    out
}

/// Serialize a slice of strings as a JSON array.
pub fn str_array(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|s| format!("\"{}\"", escape(s))).collect();
    format!("[{}]", inner.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_fields() {
        let s = "{\"method\":\"Exec\",\"id\":\"rt-1\",\"timeout_ms\":2500,\"clear\":true}";
        assert_eq!(get_str(s, "method").as_deref(), Some("Exec"));
        assert_eq!(get_str(s, "id").as_deref(), Some("rt-1"));
        assert_eq!(get_u64(s, "timeout_ms"), Some(2500));
        assert!(get_bool(s, "clear").unwrap());
        assert!(is_ok("{\"ok\":true}"));
        assert!(!is_ok("{\"ok\":false}"));
    }

    #[test]
    fn handles_escapes() {
        assert_eq!(escape("a\"b\n"), "a\\\"b\\n");
        assert_eq!(get_str("{\"p\":\"a\\\"b\"}", "p").as_deref(), Some("a\"b"));
        assert_eq!(str_array(&["a".into()]), "[\"a\"]");
    }

    #[test]
    fn parses_arrays() {
        assert_eq!(get_str_array("{\"ptys\":[\"a\",\"b\"]}", "ptys"), vec!["a", "b"]);
        assert_eq!(get_str_array("{\"ptys\":[ ]}", "ptys"), Vec::<String>::new());
    }
}
