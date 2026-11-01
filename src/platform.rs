//! Machine and user services that know nothing of a session: the user state database, the
//! secret store, the shared HTTP client, OAuth and `WWW-Authenticate` parsing, push sending,
//! the update check, the TCP bind's credentials, enrolled devices and sign-ins, and the
//! helpers for spawning processes, watching files, terminal text and titles, home-relative
//! paths and pretty-printing (docs/README.md, *Dependency direction*).

pub mod auth;
pub mod devices;
pub mod fswatch;
pub mod home;
pub mod http;
pub mod oauth;
pub mod pretty;
pub mod push;
pub mod secrets;
pub mod sign_ins;
pub mod spawn;
pub mod state;
pub mod terminal_text;
pub mod terminal_title;
pub mod update_check;
pub mod www_authenticate;
