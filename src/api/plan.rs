//! The Codex backend's subscription limits, read off response headers
//! (docs/transport.md).
//!
//! A response may carry several independently metered `x-<limit>-*` header
//! families. The ordinary Codex allowance uses `x-codex-*`; models with a
//! separate allowance, such as Codex Spark, use another family. They arrive on
//! every status, a 429 included, which is the response that most wants reading.
//! The snapshot is operational state for `GET /state`, not a record: it is never
//! appended to the log, so it needs no place in the captured-header allowlist
//! and never meets the masker.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PlanWindow {
    /// How much of the window is spent, from 0 through 100.
    pub used_percent: f64,
    /// Window length, absent when the header is missing or malformed.
    pub window_minutes: Option<u64>,
    /// Absolute Unix millis; the backend sends seconds.
    pub resets_at: Option<u64>,
}

/// The windows for one independently metered allowance.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PlanLimit {
    /// The backend's display name, when supplied.
    pub name: Option<String>,
    /// The short usage window.
    pub primary: Option<PlanWindow>,
    /// The long usage window.
    pub secondary: Option<PlanWindow>,
}

/// The plan and limits as of the most recent Codex response that carried them.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PlanSnapshot {
    /// The subscription plan type, such as `plus`.
    #[serde(rename = "type")]
    pub plan_type: Option<String>,
    /// The normalized `x-codex-active-limit`, when supplied.
    pub active: Option<String>,
    /// Independently metered allowances, keyed by normalized limit id.
    pub limits: BTreeMap<String, PlanLimit>,
    /// When this snapshot was read, as absolute Unix millis.
    pub observed_at: u64,
}

/// The last plan snapshot shared between the HTTP handler and daemon view.
pub type PlanState = Arc<Mutex<Option<PlanSnapshot>>>;

/// Construct an empty plan cell.
pub fn plan_state() -> PlanState {
    Arc::new(Mutex::new(None))
}

/// The header carrying the subscription plan type.
pub const PLAN_TYPE_HEADER: &str = "x-codex-plan-type";
/// The header identifying the currently exhausted limit.
pub const ACTIVE_LIMIT_HEADER: &str = "x-codex-active-limit";

fn normalize_limit_id(id: &str) -> String {
    id.trim().replace('-', "_")
}

fn limit_id_from_header(name: &str) -> Option<String> {
    let id = name
        .strip_prefix("x-")?
        .strip_suffix("-primary-used-percent")?;
    (!id.is_empty()).then(|| normalize_limit_id(id))
}

/// Read a snapshot from the available header names and a lookup. `None` means
/// no plan header was present, so the caller retains the previous snapshot.
pub fn plan_from_headers(
    names: impl IntoIterator<Item = impl AsRef<str>>,
    get: impl Fn(&str) -> Option<String>,
    observed_at: u64,
) -> Option<PlanSnapshot> {
    let plan_type = get(PLAN_TYPE_HEADER)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let active = get(ACTIVE_LIMIT_HEADER)
        .map(|v| normalize_limit_id(&v))
        .filter(|v| !v.is_empty());
    let limit_ids: BTreeSet<_> = names
        .into_iter()
        .filter_map(|name| limit_id_from_header(name.as_ref()))
        .collect();
    let limits: BTreeMap<String, PlanLimit> = limit_ids
        .into_iter()
        .map(|id| {
            let prefix = format!("x-{}", id.replace('_', "-"));
            let window = |kind: &str| {
                let used_percent = get(&format!("{prefix}-{kind}-used-percent"))?
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite())?;
                let number = |suffix: &str| {
                    get(&format!("{prefix}-{kind}-{suffix}"))
                        .and_then(|v| v.trim().parse::<u64>().ok())
                };
                Some(PlanWindow {
                    used_percent,
                    window_minutes: number("window-minutes"),
                    resets_at: number("reset-at").map(|secs| secs.saturating_mul(1000)),
                })
            };
            let name = get(&format!("{prefix}-limit-name"))
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty());
            (
                id,
                PlanLimit {
                    name,
                    primary: window("primary"),
                    secondary: window("secondary"),
                },
            )
        })
        .collect();
    if plan_type.is_none() && active.is_none() && limits.is_empty() {
        return None;
    }
    Some(PlanSnapshot {
        plan_type,
        active,
        limits,
        observed_at,
    })
}

#[cfg(test)]
mod test {
    use super::*;
    use std::collections::HashMap;

    fn parse(pairs: &[(&str, &str)], observed_at: u64) -> Option<PlanSnapshot> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        plan_from_headers(map.keys(), |name| map.get(name).cloned(), observed_at)
    }

    #[test]
    fn both_windows_read_with_resets_in_millis() {
        let snap = parse(
            &[
                ("x-codex-plan-type", "plus"),
                ("x-codex-primary-used-percent", "0"),
                ("x-codex-secondary-used-percent", "19"),
                ("x-codex-primary-window-minutes", "300"),
                ("x-codex-secondary-window-minutes", "10080"),
                ("x-codex-primary-reset-at", "1788677725"),
                ("x-codex-secondary-reset-at", "1789180313"),
            ],
            7,
        )
        .unwrap();
        assert_eq!(snap.plan_type.as_deref(), Some("plus"));
        let codex = &snap.limits["codex"];
        assert_eq!(
            codex.primary,
            Some(PlanWindow {
                used_percent: 0.0,
                window_minutes: Some(300),
                resets_at: Some(1_788_677_725_000),
            })
        );
        assert_eq!(codex.secondary.as_ref().unwrap().used_percent, 19.0);
        assert_eq!(snap.observed_at, 7);
    }

    #[test]
    fn separately_metered_limits_are_preserved() {
        let snap = parse(
            &[
                ("x-codex-active-limit", "codex-spark"),
                ("x-codex-primary-used-percent", "12"),
                ("x-codex-spark-primary-used-percent", "45"),
                ("x-codex-spark-primary-window-minutes", "300"),
                ("x-codex-spark-limit-name", "spark"),
            ],
            0,
        )
        .unwrap();
        assert_eq!(snap.active.as_deref(), Some("codex_spark"));
        assert_eq!(
            snap.limits["codex"].primary.as_ref().unwrap().used_percent,
            12.0
        );
        let spark = &snap.limits["codex_spark"];
        assert_eq!(spark.name.as_deref(), Some("spark"));
        assert_eq!(spark.primary.as_ref().unwrap().used_percent, 45.0);
    }

    #[test]
    fn malformed_values_are_missing() {
        let snap = parse(
            &[
                ("x-codex-plan-type", "plus"),
                ("x-codex-primary-used-percent", "42.5"),
                ("x-codex-primary-window-minutes", "soon"),
                ("x-codex-secondary-used-percent", "lots"),
            ],
            0,
        )
        .unwrap();
        let codex = &snap.limits["codex"];
        let primary = codex.primary.as_ref().unwrap();
        assert_eq!(primary.used_percent, 42.5);
        assert_eq!(primary.window_minutes, None);
        assert!(codex.secondary.is_none());
    }

    #[test]
    fn no_plan_headers_is_none() {
        assert!(parse(&[("content-type", "text/event-stream")], 0).is_none());
    }

    #[test]
    fn serializes_named_limits_and_absolute_resets() {
        let snap = PlanSnapshot {
            plan_type: Some("plus".into()),
            active: None,
            limits: BTreeMap::from([(
                "codex".into(),
                PlanLimit {
                    name: None,
                    primary: Some(PlanWindow {
                        used_percent: 12.0,
                        window_minutes: Some(300),
                        resets_at: Some(1_788_677_725_000),
                    }),
                    secondary: None,
                },
            )]),
            observed_at: 1,
        };
        assert_eq!(
            serde_json::to_string(&snap).unwrap(),
            r#"{"type":"plus","active":null,"limits":{"codex":{"name":null,"primary":{"used_percent":12.0,"window_minutes":300,"resets_at":1788677725000},"secondary":null}},"observed_at":1}"#
        );
    }
}
