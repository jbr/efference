//! The Codex backend's subscription windows, read off response headers
//! (docs/transport.md).
//!
//! Every response from `chatgpt.com/backend-api/codex/responses` carries
//! `x-codex-*` headers naming the plan and two usage windows — a 5-hour primary
//! and a 7-day secondary — each as a used percentage, a window length, and a
//! reset epoch. They arrive on every status, a 429 included, which is the response
//! that most wants reading. The snapshot is operational state for `GET /state`,
//! not a record: it is never appended to the log, so it needs no place in the
//! captured-header allowlist and never meets the masker.

use std::sync::{Arc, Mutex};

use serde::Serialize;

/// One usage window as the backend reports it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PlanWindow {
    /// How much of the window is spent, 0–100.
    pub used_percent: f64,
    /// The window's length; absent when the header is missing or unparseable.
    pub window_minutes: Option<u64>,
    /// When the window resets, as absolute Unix millis (the backend sends
    /// seconds; every other instant on the control API is millis). Absolute so
    /// the topic does not tick: clients subtract from their own clock.
    pub resets_at: Option<u64>,
}

/// The plan and its windows as of the most recent Codex response that carried
/// them. A response without any of the headers leaves the previous snapshot in
/// place; one with any of them replaces the whole snapshot, so a window the
/// backend stopped reporting reads as absent rather than stale.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PlanSnapshot {
    /// `x-codex-plan-type`, e.g. `plus`.
    #[serde(rename = "type")]
    pub plan_type: Option<String>,
    /// The short window (5 hours on a Plus plan).
    pub primary: Option<PlanWindow>,
    /// The long window (7 days on a Plus plan).
    pub secondary: Option<PlanWindow>,
    /// When this snapshot was read, absolute Unix millis.
    pub observed_at: u64,
}

/// The latest snapshot, written by the HTTP client's response handler and read
/// by the daemon's `usage` topic. `None` until a Codex response arrives, and
/// always `None` on any other provider.
pub type PlanState = Arc<Mutex<Option<PlanSnapshot>>>;

/// Construct an empty plan cell.
pub fn plan_state() -> PlanState {
    Arc::new(Mutex::new(None))
}

/// The header names read, so a test and the handler agree on the set.
pub const PLAN_TYPE_HEADER: &str = "x-codex-plan-type";

/// Read a snapshot off a header lookup. `None` when no plan header is present at
/// all, which the caller treats as "retain the previous snapshot". A present but
/// unparseable value is a missing field, never a failed response.
pub fn plan_from_headers(
    get: impl Fn(&str) -> Option<String>,
    observed_at: u64,
) -> Option<PlanSnapshot> {
    let window = |name: &str| {
        let used_percent = get(&format!("x-codex-{name}-used-percent"))?
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())?;
        let number = |suffix: &str| {
            get(&format!("x-codex-{name}-{suffix}")).and_then(|v| v.trim().parse::<u64>().ok())
        };
        Some(PlanWindow {
            used_percent,
            window_minutes: number("window-minutes"),
            resets_at: number("reset-at").map(|secs| secs.saturating_mul(1000)),
        })
    };
    let plan_type = get(PLAN_TYPE_HEADER)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let primary = window("primary");
    let secondary = window("secondary");
    if plan_type.is_none() && primary.is_none() && secondary.is_none() {
        return None;
    }
    Some(PlanSnapshot {
        plan_type,
        primary,
        secondary,
        observed_at,
    })
}

#[cfg(test)]
mod test {
    use super::*;
    use std::collections::HashMap;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name: &str| map.get(name).cloned()
    }

    /// The header set a curl of the backend returns, both windows present.
    #[test]
    fn both_windows_read_with_resets_in_millis() {
        let snap = plan_from_headers(
            lookup(&[
                ("x-codex-plan-type", "plus"),
                ("x-codex-primary-used-percent", "0"),
                ("x-codex-secondary-used-percent", "19"),
                ("x-codex-primary-window-minutes", "300"),
                ("x-codex-secondary-window-minutes", "10080"),
                ("x-codex-primary-reset-at", "1788677725"),
                ("x-codex-secondary-reset-at", "1789180313"),
            ]),
            7,
        )
        .unwrap();
        assert_eq!(snap.plan_type.as_deref(), Some("plus"));
        assert_eq!(
            snap.primary,
            Some(PlanWindow {
                used_percent: 0.0,
                window_minutes: Some(300),
                resets_at: Some(1_788_677_725_000),
            })
        );
        assert_eq!(snap.secondary.as_ref().unwrap().used_percent, 19.0);
        assert_eq!(snap.secondary.as_ref().unwrap().window_minutes, Some(10080));
        assert_eq!(snap.observed_at, 7);
    }

    #[test]
    fn a_malformed_field_is_missing_and_a_malformed_percent_drops_its_window() {
        let snap = plan_from_headers(
            lookup(&[
                ("x-codex-plan-type", "plus"),
                ("x-codex-primary-used-percent", "42.5"),
                ("x-codex-primary-window-minutes", "soon"),
                ("x-codex-secondary-used-percent", "lots"),
                ("x-codex-secondary-window-minutes", "10080"),
            ]),
            0,
        )
        .unwrap();
        let primary = snap.primary.unwrap();
        assert_eq!(primary.used_percent, 42.5);
        assert_eq!(primary.window_minutes, None);
        assert_eq!(primary.resets_at, None);
        assert!(snap.secondary.is_none());
    }

    #[test]
    fn no_plan_headers_is_none() {
        assert!(plan_from_headers(lookup(&[("content-type", "text/event-stream")]), 0).is_none());
    }

    #[test]
    fn serializes_with_type_and_absolute_resets() {
        let snap = PlanSnapshot {
            plan_type: Some("plus".into()),
            primary: Some(PlanWindow {
                used_percent: 12.0,
                window_minutes: Some(300),
                resets_at: Some(1_788_677_725_000),
            }),
            secondary: None,
            observed_at: 1,
        };
        assert_eq!(
            serde_json::to_string(&snap).unwrap(),
            r#"{"type":"plus","primary":{"used_percent":12.0,"window_minutes":300,"resets_at":1788677725000},"secondary":null,"observed_at":1}"#
        );
    }
}
