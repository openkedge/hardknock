// SPDX-License-Identifier: Apache-2.0
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

static QUOTED_ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)([A-Z0-9_]*(?:TOKEN|KEY|SECRET|PASSWORD)|authorization)(["']?\s*[:=]\s*)(?:"[^"]*"|'[^']*')"#).expect("constant regex")
});

static ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)([A-Z0-9_]*(?:TOKEN|KEY|SECRET|PASSWORD)|authorization)([\"']?\s*[:=]\s*[\"']?)(?:bearer\s+)?[^\s\"',;]+"#).expect("constant regex")
});
static BEARER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)bearer\s+[A-Za-z0-9._~+/=-]+").expect("constant regex"));
static KNOWN_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:sk-(?:proj-|ant-)?[A-Za-z0-9_-]{12,}|AKIA[A-Z0-9]{16})\b")
        .expect("constant regex")
});

pub fn bounded(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.into();
    }
    let mut end = max_bytes.saturating_sub("…[truncated]".len());
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}…[truncated]", &text[..end])
}
pub fn redact(text: &str, max_bytes: usize) -> String {
    // Redact before truncation, including secrets crossing the truncation boundary.
    let text = QUOTED_ASSIGNMENT.replace_all(text, "${1}${2}[REDACTED]");
    let text = ASSIGNMENT.replace_all(&text, "${1}${2}[REDACTED]");
    let text = BEARER.replace_all(&text, "Bearer [REDACTED]");
    bounded(&KNOWN_KEY.replace_all(&text, "[REDACTED]"), max_bytes)
}
pub fn sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_uppercase();
    [
        "TOKEN",
        "KEY",
        "SECRET",
        "PASSWORD",
        "AUTHORIZATION",
        "COOKIE",
    ]
    .iter()
    .any(|suffix| key.ends_with(suffix))
}
pub fn redact_value(value: &mut Value) {
    match value {
        Value::String(s) => *s = redact(s, 8192),
        Value::Array(a) => {
            a.truncate(128);
            for v in a {
                redact_value(v);
            }
        }
        Value::Object(o) => {
            for (k, v) in o {
                if sensitive_key(k)
                    || matches!(
                        k.as_str(),
                        "prompt"
                            | "messages"
                            | "conversation"
                            | "transcript"
                            | "reasoning"
                            | "chain_of_thought"
                            | "content"
                    )
                {
                    *v = Value::String("[REDACTED]".into());
                } else {
                    redact_value(v);
                }
            }
        }
        _ => {}
    }
}
