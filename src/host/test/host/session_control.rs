//! The in-session `agents__*` server a host builds for its sessions
//! (`host/session_control.rs`, docs/tools.md): none until the process serving the host
//! installs a mount, and then the in-session set.

use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_builds_the_in_session_server_once_a_mount_is_installed() {
    let host = Host::new(crate::inference::InferenceRegistry::empty(), baseline_env());
    assert!(
        host.session_control_server("caller").is_none(),
        "a host nothing was installed on boots its sessions without the verbs"
    );

    host.set_session_control(crate::clients::control_tools::in_session_server);
    let server = host
        .session_control_server("caller")
        .expect("a mount is installed");
    assert!(server.handles("agents__message"));
    assert!(server.handles("agents__hand_off"));
    assert!(
        !server.handles("agents__wait"),
        "`wait` is `efference mcp`'s, not a session's"
    );
}
