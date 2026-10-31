//! What the web transport answers a browser signing in (docs/web-bind.md): the code it
//! shows and the claim it polls with, and the claim's answer; and what the socket's
//! enrollment routes answer the operator, who approves a sign-in or enrolls a device.
//!
//! The pending sign-ins, the approval and the enrollment that mints a token are the host's
//! (`sign_ins`, `devices`, `daemon::tcp`).

use serde::{Deserialize, Serialize};

use crate::devices::{Device, DeviceId};
use crate::protocol::answers::StatusBody;
use crate::sign_ins::Pending;

/// What `POST /pair` answers the page that asked: the claim secret this page alone polls
/// with, the short code it shows for the operator to approve at the host, and the seconds
/// both live.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SignInRequest {
    pub claim: String,
    pub code: String,
    pub expires_in: i64,
}

/// A claim's answer from `POST /pair/claim`: the device token once the host approved, or
/// word that it has not yet. A claim that names nothing (used, expired, displaced) is a
/// `404` instead.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum SignInClaim {
    /// Approved, and this was the claim's first: the token the page keeps.
    Approved { token: String },
    /// Live and not yet approved: the page asks again.
    Waiting(StatusBody),
}

/// The sign-in link from `GET /auth/link`: a sign-in approved before it was asked for, its
/// claim secret in the fragment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SignInLink {
    pub url: String,
}

/// The sign-ins waiting for approval, oldest first, from `GET /auth/pending`. A row has
/// the label and the time but never the code: approval is typing what the browser shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct PendingSignInList {
    pub pending: Vec<Pending>,
}

/// The enrolled devices, oldest first, from `GET /auth/devices`. Never a token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct DeviceList {
    pub devices: Vec<Device>,
}

/// The answer to `DELETE /auth/devices/:id`: the device whose token is refused from now on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct RemovedDevice {
    pub removed: DeviceId,
}

#[cfg(test)]
mod test;
