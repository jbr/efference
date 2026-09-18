//! `QUERY /sessions`: the lenient half of the session-options surface
//! (docs/control-api.md). Creation deserializes straight into [`CreateSession`]
//! and a wrong type is a `400`; preview cannot do that, because a form's whole
//! reason to ask is that one of its fields is mid-edit. So the body is read out
//! of a `serde_json::Value` field by field, and each failure becomes an issue
//! beside the fields that still resolved.
//!
//! Only the decoding differs. Both routes resolve through
//! [`InferenceRegistry::resolve_options`](crate::inference::InferenceRegistry::resolve_options),
//! so what preview reports is what creation builds.

use super::*;
use crate::inference::{Issue, IssueCode, SessionOptions};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The body of `QUERY /sessions` (docs/control-api.md): where the proposed
/// session would run, plus the same editable selections creation accepts.
#[derive(Serialize, Deserialize)]
pub struct SessionOptionsRequest {
    /// The proposed session's project root. Absolute, as creation's is: it is
    /// what a relative `config` resolves against.
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<PathBuf>,
    #[serde(flatten)]
    pub options: SessionOptions,
}

impl SessionOptionsRequest {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            config: None,
            options: SessionOptions::default(),
        }
    }
}

/// A preview body decoded field by field. Preview is deliberately lenient where
/// creation is strict: one ill-typed value must not stop the other fields from
/// resolving, so nothing here deserializes straight into its destination type.
struct DecodedOptions {
    cwd: Option<PathBuf>,
    config: Option<PathBuf>,
    options: SessionOptions,
    issues: Vec<Issue>,
}

fn options_string(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &'static str,
    issues: &mut Vec<Issue>,
) -> Option<String> {
    match object.get(field) {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(value)) => Some(value.clone()),
        Some(_) => {
            issues.push(Issue::new(
                field,
                IssueCode::InvalidType,
                "must be a string",
            ));
            None
        }
    }
}

fn decode_session_options(body: &serde_json::Value) -> Option<DecodedOptions> {
    let object = body.as_object()?;
    let mut issues = Vec::new();
    let cwd = match object.get("cwd") {
        None | Some(serde_json::Value::Null) => {
            issues.push(Issue::new(
                "cwd",
                IssueCode::Required,
                "must be an absolute path",
            ));
            None
        }
        Some(serde_json::Value::String(path)) => {
            let path = PathBuf::from(path);
            if path.is_absolute() {
                Some(path)
            } else {
                issues.push(Issue::new(
                    "cwd",
                    IssueCode::InvalidSelection,
                    "must be an absolute path",
                ));
                None
            }
        }
        Some(_) => {
            issues.push(Issue::new(
                "cwd",
                IssueCode::InvalidType,
                "must be an absolute path string",
            ));
            None
        }
    };
    let config = options_string(object, "config", &mut issues).map(PathBuf::from);
    let triage = match object.get("triage") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => match serde_json::from_value(value.clone()) {
            Ok(choice) => Some(choice),
            Err(_) => {
                issues.push(Issue::new(
                    "triage",
                    IssueCode::InvalidType,
                    "must be {\"mode\": \"shared\"} or {\"mode\": \"separate\", \"model\": …}",
                ));
                None
            }
        },
    };
    // The accepted range is the resolver's rule and is checked there; this arm
    // only has to get a number out of JSON without serde rejecting the whole
    // body, which it would for anything past a `u8`.
    let temperature_message = format!(
        "must be a whole number from {} to {}",
        crate::inference::TEMPERATURE.start(),
        crate::inference::TEMPERATURE.end()
    );
    let temperature = match object.get("temperature") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::Number(value)) => {
            match value.as_u64().and_then(|value| u8::try_from(value).ok()) {
                Some(value) => Some(value),
                None => {
                    issues.push(Issue::new(
                        "temperature",
                        IssueCode::OutOfRange,
                        temperature_message,
                    ));
                    None
                }
            }
        }
        Some(_) => {
            issues.push(Issue::new(
                "temperature",
                IssueCode::InvalidType,
                temperature_message,
            ));
            None
        }
    };
    Some(DecodedOptions {
        cwd,
        config,
        options: SessionOptions {
            connection: options_string(object, "connection", &mut issues),
            model: options_string(object, "model", &mut issues),
            spotter_connection: options_string(object, "spotter_connection", &mut issues),
            spotter_model: options_string(object, "spotter_model", &mut issues),
            triage,
            effort: options_string(object, "effort", &mut issues),
            temperature,
        },
        issues,
    })
}

/// `QUERY /sessions`: resolve a proposed session without minting a directory,
/// reading credentials, or preparing runtime resources. The only file it touches
/// is the project config it would select, read to answer where effort comes from.
pub(super) async fn preview_session(mut conn: Conn) -> Conn {
    let body = conn.request_body_string().await.unwrap_or_default();
    let body: serde_json::Value = match serde_json::from_str(&body) {
        Ok(body) => body,
        Err(error) => {
            return fail(
                conn,
                ErrorCode::BadRequest,
                format!("invalid JSON: {error}"),
            );
        }
    };
    let Some(decoded) = decode_session_options(&body) else {
        return fail(
            conn,
            ErrorCode::BadRequest,
            "session options must be a JSON object",
        );
    };
    let Some(host) = host_of(&conn) else {
        return missing_host(conn);
    };
    let response = session_options_response(&host, decoded);
    conn.with_status(200).with_body(response).halt()
}

fn session_options_response(host: &Host, decoded: DecodedOptions) -> String {
    let DecodedOptions {
        cwd,
        config,
        options,
        mut issues,
    } = decoded;
    // Effort can be inherited from the project config, so preview reads the one
    // creation would select. A config that will not load is an issue, never its
    // contents or the parser's view of them.
    let project_effort = match cwd {
        Some(cwd) => match crate::config::load_in(&cwd, config.as_deref()) {
            Ok(loaded) => loaded.role.transport.effort,
            Err(_) => {
                issues.push(Issue::new(
                    "config",
                    IssueCode::InvalidConfig,
                    "could not read or parse the selected config",
                ));
                None
            }
        },
        // No usable cwd: its own issue already says so, and nothing about the
        // roles depends on it.
        None => None,
    };
    let mut resolved = host
        .inference
        .resolve_options(&options, project_effort.as_deref());
    // A field the decoder rejected never reached resolution, which would report
    // it as an ordinary omission. Blank it, so every issue's field reads the same.
    for issue in &issues {
        resolved.invalidate(issue.field);
    }
    issues.append(&mut resolved.issues);
    resolved.issues = issues;
    serde_json::to_string(&resolved).unwrap_or_else(|_| {
        // Every field is plain data; a failure here would be a serde bug, not a
        // request the caller can fix.
        String::from(r#"{"effective":null,"issues":[]}"#)
    })
}
