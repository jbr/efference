//! The contract between a session and whatever drives it: the names and shapes that cross
//! the control channel or the tool wire (docs/control-api.md, docs/tools.md).
//!
//! Each submodule holds one surface's types and names, and the readers of them that need
//! nothing else, such as which decision a call's input names. The code that serves a
//! surface imports its contract from here, and so does the code that only recognizes it:
//! the loop, the toolbox, the config validator. The module imports from `log` and
//! `policy` and from no module that serves a surface (docs/README.md, *Dependency
//! direction*).

pub mod agents;
