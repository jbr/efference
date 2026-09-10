//! Tests for `approvals.rs`.

use super::*;

/// A gate surfaces the spotter's reason on `/state` (docs/permissions.md): the spotter logs
/// its verdict as a `Spot` *before* routing to the human, so `register_pending` reads the
/// reason from the log — no side channel. The client renders it inline at the gate.
#[test]
fn the_gate_carries_the_spotter_reason() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let call = bash_call("rm -rf build");
    shared
        .append(
            0,
            Payload::Spot(crate::log::Spot {
                call_id: call.id.clone(),
                stance: crate::log::Stance::Gate,
                reason: "deletes outside the git tree — not reversible".into(),
                origin: crate::log::Origin::Model,
                probed_prefix: None,
            }),
        )
        .unwrap();
    let _gate = shared.register_pending(&call);
    assert!(
        shared
            .state_json()
            .contains("deletes outside the git tree — not reversible"),
        "the gate's /state should carry the spotter's reason",
    );
}
