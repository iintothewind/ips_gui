//! Lenient JSON for metadata written by other tools. JPEG metadata is capped at one 64 KB
//! segment, so a ComfyUI graph stored in EXIF UserComment can arrive cut off; and ComfyUI
//! serializes with Python's `json.dumps`, which writes non-finite floats as bare `NaN`,
//! `Infinity` and `-Infinity`.

use serde_json::Value;

/// Parse `text` as JSON. When the document ends early, keep every value that was complete
/// before the cut and close the open objects and arrays around them. Python's non-finite
/// float tokens are read as `null`.
/// Returns the value and whether it had to be repaired; `None` when nothing is recoverable
/// or the text is malformed for another reason.
pub fn parse_lenient(text: &str) -> Option<(Value, bool)> {
    parse_or_close(text).or_else(|| parse_or_close(&replace_non_finite(text)?))
}

fn parse_or_close(text: &str) -> Option<(Value, bool)> {
    match serde_json::from_str(text) {
        Ok(value) => Some((value, false)),
        Err(e) if e.is_eof() => {
            let repaired = close_truncated(text)?;
            serde_json::from_str(&repaired).ok().map(|value| (value, true))
        }
        Err(_) => None,
    }
}

/// `text` with `NaN`, `Infinity` and `-Infinity` outside strings replaced by `null`, or
/// `None` when there are none.
fn replace_non_finite(text: &str) -> Option<String> {
    const TOKENS: [&str; 3] = ["-Infinity", "Infinity", "NaN"];
    let mut out = String::with_capacity(text.len());
    let mut replaced = false;
    let mut in_string = false;
    let mut escaped = false;
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if !in_string {
            if let Some(token) = TOKENS.iter().find(|token| rest.starts_with(**token)) {
                out.push_str("null");
                rest = &rest[token.len()..];
                replaced = true;
                continue;
            }
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    replaced.then_some(out)
}

#[derive(Clone, Copy)]
enum Frame {
    /// `expect_key`: the next string in this object is a member name, not a value.
    Object { expect_key: bool },
    Array,
}

/// Cut `text` after the last complete value inside a container and append the closers of
/// every container still open there. A trailing key, partial string, number or literal is
/// dropped because its full content is unknown.
fn close_truncated(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut stack: Vec<Frame> = Vec::new();
    // (cut position, closers for the containers open at that position)
    let mut safe: Option<(usize, String)> = None;
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i += 1;
                loop {
                    match bytes.get(i) {
                        None => return finish(text, safe),
                        Some(b'\\') => i += 2,
                        Some(b'"') => break,
                        Some(_) => i += 1,
                    }
                }
                let is_key = matches!(stack.last(), Some(Frame::Object { expect_key: true }));
                if !is_key && !stack.is_empty() {
                    safe = Some((i + 1, closers(&stack)));
                }
            }
            b'{' => {
                stack.push(Frame::Object { expect_key: true });
                safe = Some((i + 1, closers(&stack)));
            }
            b'[' => {
                stack.push(Frame::Array);
                safe = Some((i + 1, closers(&stack)));
            }
            b'}' | b']' => {
                stack.pop();
                if !stack.is_empty() {
                    safe = Some((i + 1, closers(&stack)));
                }
            }
            b':' => {
                if let Some(Frame::Object { expect_key }) = stack.last_mut() {
                    *expect_key = false;
                }
            }
            b',' => {
                if !stack.is_empty() {
                    safe = Some((i, closers(&stack)));
                }
                if let Some(Frame::Object { expect_key }) = stack.last_mut() {
                    *expect_key = true;
                }
            }
            _ => {}
        }
        i += 1;
    }

    finish(text, safe)
}

fn finish(text: &str, safe: Option<(usize, String)>) -> Option<String> {
    let (cut, closers) = safe?;
    Some(format!("{}{}", &text[..cut], closers))
}

fn closers(stack: &[Frame]) -> String {
    stack
        .iter()
        .rev()
        .map(|frame| match frame {
            Frame::Object { .. } => '}',
            Frame::Array => ']',
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn complete_json_is_not_repaired() {
        assert_eq!(parse_lenient(r#"{"a":1}"#), Some((json!({"a": 1}), false)));
    }

    #[test]
    fn keeps_complete_values_before_the_cut() {
        let cases = [
            (r#"{"a":{"b":"x","c":[1,2"#, json!({"a": {"b": "x", "c": [1]}})),
            (r#"{"a":"done","b":"partial str"#, json!({"a": "done"})),
            (r#"{"a":"done","b"#, json!({"a": "done"})),
            (r#"{"a":"done","b":"#, json!({"a": "done"})),
            (r#"{"a":[{"k":"v"}],"b":tr"#, json!({"a": [{"k": "v"}]})),
            (r#"{"a":"esc \" quote","b":{"#, json!({"a": "esc \" quote", "b": {}})),
            (r#"{"#, json!({})),
        ];
        for (text, expected) in cases {
            assert_eq!(parse_lenient(text), Some((expected, true)), "{text}");
        }
    }

    #[test]
    fn python_non_finite_floats_read_as_null() {
        let text = r#"{"cfg":NaN,"denoise":Infinity,"shift":-Infinity,"note":"\"NaN\" stays text"}"#;
        assert_eq!(
            parse_lenient(text),
            Some((json!({"cfg": null, "denoise": null, "shift": null, "note": "\"NaN\" stays text"}), false))
        );
        assert_eq!(
            parse_lenient(r#"{"a":NaN,"b":"cut"#),
            Some((json!({"a": null}), true))
        );
    }

    #[test]
    fn malformed_or_unrecoverable_text_is_rejected() {
        assert_eq!(parse_lenient(r#"{"a":1}}"#), None);
        assert_eq!(parse_lenient(r#""unterminated"#), None);
        assert_eq!(parse_lenient("not json"), None);
    }
}
