// SPDX-License-Identifier: Apache-2.0
use super::ExperienceBundle;
use crate::{Error, Result};
use regex::Regex;
use serde_json::Value;
use std::path::Path;

pub trait FederationRedactionPolicy {
    fn redact(&self, bundle: ExperienceBundle) -> Result<ExperienceBundle>;
}
pub struct DeterministicFederationRedaction<'a> {
    pub repository: Option<&'a Path>,
}
impl DeterministicFederationRedaction<'_> {
    fn redact_string(&self, value: &str) -> Result<String> {
        if value.contains('\0') {
            return Err(Error::InvalidInput(
                "Federation payload contains a NUL byte".into(),
            ));
        }
        let mut output = value.to_owned();
        if let Some(repo) = self.repository {
            output = output.replace(&repo.to_string_lossy().to_string(), "$REPO");
        }
        let home = std::env::var("HOME").unwrap_or_default();
        if !home.is_empty() {
            output = output.replace(&home, "$HOME");
        }
        let home_path = Regex::new(r"/(?:Users|home)/[^/\s]+/")
            .map_err(|e| Error::InvalidInput(e.to_string()))?;
        output = home_path.replace_all(&output, "$HOME/").into_owned();
        let authorization = Regex::new(r"(?i)authorization\s*:\s*(?:bearer|basic)\s+[^\s,;]+")
            .map_err(|e| Error::InvalidInput(e.to_string()))?;
        output = authorization
            .replace_all(&output, "Authorization: [REDACTED]")
            .into_owned();
        let assignment=Regex::new(r"(?i)\b(api[_-]?(?:key|token)|access[_-]?token|auth[_-]?token|[a-z][a-z0-9_]*(?:token|secret_access_key|secret|password|api_key|access_key)|password|passwd|secret(?:_access_key)?)\b\s*[:=]\s*[^\s,;]+") .map_err(|e|Error::InvalidInput(e.to_string()))?;
        output = assignment
            .replace_all(&output, "$1=[REDACTED]")
            .into_owned();
        let aws = Regex::new(r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b")
            .map_err(|e| Error::InvalidInput(e.to_string()))?;
        output = aws.replace_all(&output, "[REDACTED_AWS_KEY]").into_owned();
        let jwt = Regex::new(r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b")
            .map_err(|e| Error::InvalidInput(e.to_string()))?;
        output = jwt.replace_all(&output, "[REDACTED_TOKEN]").into_owned();
        Ok(output)
    }
    fn walk(&self, value: &mut Value) -> Result<()> {
        match value {
            Value::String(s) => *s = self.redact_string(s)?,
            Value::Array(a) => {
                for v in a {
                    self.walk(v)?
                }
            }
            Value::Object(map) => {
                for (key, v) in map {
                    if is_secret_name(key) {
                        *v = Value::String("[REDACTED]".into())
                    } else {
                        self.walk(v)?
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}
fn is_secret_name(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "api_token"
            | "api_key"
            | "aws_secret_access_key"
            | "aws_access_key_id"
            | "authorization"
            | "password"
            | "passwd"
            | "secret"
            | "access_token"
            | "auth_token"
    )
}
impl FederationRedactionPolicy for DeterministicFederationRedaction<'_> {
    fn redact(&self, bundle: ExperienceBundle) -> Result<ExperienceBundle> {
        let mut value = serde_json::to_value(bundle)?;
        self.walk(&mut value)?;
        Ok(serde_json::from_value(value)?)
    }
}

pub fn validate_safe_payload(value: &Value, max_depth: usize) -> Result<()> {
    fn walk(value: &Value, depth: usize, max: usize) -> Result<()> {
        if depth > max {
            return Err(Error::InvalidInput(
                "Bundle nesting depth limit exceeded".into(),
            ));
        }
        match value {
            Value::String(s) => {
                if s.contains('\0')
                    || s.contains("../")
                    || s.contains("..\\")
                    || s.starts_with("/Users/")
                    || s.starts_with("/home/")
                {
                    return Err(Error::InvalidInput(
                        "Unsafe path or NUL byte in bundle".into(),
                    ));
                }
            }
            Value::Array(a) => {
                for v in a {
                    walk(v, depth + 1, max)?
                }
            }
            Value::Object(m) => {
                for (k, v) in m {
                    if is_secret_name(k) && v.as_str() != Some("[REDACTED]") {
                        return Err(Error::InvalidInput(
                            "Secret-bearing field was not redacted".into(),
                        ));
                    }
                    walk(v, depth + 1, max)?
                }
            }
            _ => {}
        }
        Ok(())
    }
    walk(value, 0, max_depth)
}
