//! The control channel's routes and the readouts they answer from: the typed error envelope,
//! the frame envelope and the unified control stream, the serialization of a log entry for a
//! client, the per-session route families (`http`) and the host's own handlers (`host_http`)
//! (docs/README.md, *Dependency direction*).

pub(crate) mod entry_view;
// The typed error envelope (docs/control-api.md) — every non-2xx body the router
// answers with.
pub(crate) mod errors;
// The control-stream frame envelope (docs/control-api.md). `dead_code` covers
// `Frame::with_render` and `Frame::state`, which the emitter does not reach:
// nothing annotates a frame with a render yet, and the state path builds from
// pre-serialized topic JSON (`Frame::state_raw`).
#[allow(dead_code)]
pub(crate) mod frames;
pub(crate) mod host_http;
pub(crate) mod http;
pub(crate) mod stream;
pub(crate) mod views;
