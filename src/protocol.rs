//! The contract between a session and whatever drives it: the names and shapes that cross
//! the control channel or the tool wire (docs/control-api.md, docs/tools.md).
//!
//! Each submodule holds one surface's types and names, and the readers of them that need
//! nothing else, such as which decision a call's input names. The code that serves a
//! surface imports its contract from here, and so does the code that only reads it: the
//! clients, the loop, the toolbox, the config validator. Behaviour that needs a subsystem
//! stays with that subsystem, as `impl` blocks on the types defined here.
//!
//! The module imports the vocabulary its shapes are written in — `log`, `policy`,
//! `config`, `inference`, `inventory`, `api`, `process_monitor`, `project` — and nothing
//! from a module that hosts sessions or is a client of them (docs/README.md, *Dependency
//! direction*).

pub mod agents;
pub mod attention;
pub mod sessions;
pub mod state;
pub mod status;
pub mod stream;
pub mod transcript;
pub mod turns;
