//! The secret store (docs/secrets.md): `get`/`set`/`delete` of one named string
//! in the user's OS credential store, under the service name `efference`.
//!
//! Three populations share it under prefixed names — `inference:<connection>`
//! for API keys, `oauth:<connection>` for ChatGPT grants, and `mcp:<server>`
//! for MCP grants — so a listing in the OS's own UI reads, and none can shadow
//! another.
//!
//! The backend is chosen once per process by [`Secrets::detect`]:
//!
//! - **macOS, identity-signed binary**: the login keychain in-process. A
//!   keychain item's access list names the code that stored it by its
//!   designated requirement, which for an identity-signed binary is stable
//!   across rebuilds; an ad-hoc signature's requirement is the code hash, so
//!   every `cargo build` would be a new, prompting reader.
//! - **macOS, ad-hoc**: the `security` tool, whose own access-list entry is
//!   what every dev build shares. Items it wrote it reads with no prompt.
//!   Writes go through `security -i` on stdin so the value never sits on argv.
//! - **Linux**: Secret Service in-process; nothing there binds to the binary.
//!
//! Detection connects to nothing. The in-process store is opened by the first
//! `get`/`set`/`delete`, so a process that never touches an item never needs
//! a Secret Service, and one that does fails that call, naming the item.
//!
//! Native calls, and the open, run on a fresh thread: the Secret Service store
//! drives its own async runtime, which cannot be entered from a tokio worker.

use std::collections::BTreeMap;
use std::sync::Mutex;

#[cfg(test)]
mod test;

/// The service every item is filed under.
pub const SERVICE: &str = "efference";

/// The name an inference connection's key is stored under.
pub fn inference_name(connection: &str) -> String {
    format!("inference:{connection}")
}

/// The name a connection's OAuth grant is stored under. Distinct from
/// [`inference_name`], whose item is a bare secret: this one holds a JSON
/// record (docs/secrets.md), and a human reading the store should not have to
/// guess which a given item is.
pub fn oauth_name(connection: &str) -> String {
    format!("oauth:{connection}")
}

/// The name an MCP server's grant is stored under.
pub fn mcp_name(server: &str) -> String {
    format!("mcp:{server}")
}

/// A failed store operation: what was attempted and the store's own words.
/// Never carries a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretError {
    pub operation: &'static str,
    pub name: String,
    pub detail: String,
}

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "could not {} secret `{}`: {}",
            self.operation, self.name, self.detail
        )
    }
}

impl std::error::Error for SecretError {}

/// Opens the platform store if this process has not yet ([`native::open`]).
/// Called before every native operation, and cheap once the store is open
/// (docs/secrets.md).
type Open = fn() -> Result<(), String>;

enum Backend {
    /// `keyring-core` over the platform store, opened by `open` on the first
    /// operation that needs it.
    Native { open: Open },
    /// macOS `security(1)`.
    #[cfg(target_os = "macos")]
    SecurityCli,
    /// Tests: a map, nothing leaves the process.
    Memory(Mutex<BTreeMap<String, String>>),
}

/// One handle on the store. Cheap to construct: detection connects to nothing,
/// and the platform store is opened once per process, by the first operation.
pub struct Secrets {
    backend: Backend,
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secrets")
            .field("backend", &self.describe())
            .finish()
    }
}

impl Secrets {
    /// The store for this platform and this binary's signature. Chooses a
    /// backend without contacting it: a store that cannot be opened fails the
    /// operations that need it, not detection.
    pub fn detect() -> Self {
        #[cfg(target_os = "macos")]
        if !identity_signed() {
            return Self {
                backend: Backend::SecurityCli,
            };
        }
        Self::native(native::open)
    }

    /// The in-process platform store, opened by `open` when first used.
    fn native(open: Open) -> Self {
        Self {
            backend: Backend::Native { open },
        }
    }

    /// Open the store now rather than at the first operation, for a caller
    /// that offers to write only where writing can work (setup). The reason
    /// is the store's own words, with no item to name.
    pub fn open(&self) -> Result<(), String> {
        match &self.backend {
            Backend::Native { open } => on_fresh_thread(*open),
            #[cfg(target_os = "macos")]
            Backend::SecurityCli => Ok(()),
            Backend::Memory(_) => Ok(()),
        }
    }

    /// A store that lives in this process only.
    pub fn in_memory() -> Self {
        Self {
            backend: Backend::Memory(Mutex::new(BTreeMap::new())),
        }
    }

    /// Where values go, for a prompt or a report: the store, not any item.
    pub fn describe(&self) -> &'static str {
        match &self.backend {
            Backend::Native { .. } => native::DESCRIPTION,
            #[cfg(target_os = "macos")]
            Backend::SecurityCli => "the login keychain, through `security`",
            Backend::Memory(_) => "process memory",
        }
    }

    pub fn get(&self, name: &str) -> Result<Option<String>, SecretError> {
        let wrap = |detail| SecretError {
            operation: "read",
            name: name.to_string(),
            detail,
        };
        match &self.backend {
            Backend::Native { open } => native_call(*open, name, native::get).map_err(wrap),
            #[cfg(target_os = "macos")]
            Backend::SecurityCli => security_cli::get(name).map_err(wrap),
            Backend::Memory(map) => Ok(map.lock().unwrap().get(name).cloned()),
        }
    }

    /// Store `value` under `name`, replacing any previous value.
    pub fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
        let wrap = |detail| SecretError {
            operation: "write",
            name: name.to_string(),
            detail,
        };
        match &self.backend {
            Backend::Native { open } => {
                let value = value.to_string();
                native_call(*open, name, move |name| native::set(name, &value)).map_err(wrap)
            }
            #[cfg(target_os = "macos")]
            Backend::SecurityCli => security_cli::set(name, value).map_err(wrap),
            Backend::Memory(map) => {
                map.lock()
                    .unwrap()
                    .insert(name.to_string(), value.to_string());
                Ok(())
            }
        }
    }

    /// Remove `name`. Absent is not an error.
    pub fn delete(&self, name: &str) -> Result<(), SecretError> {
        let wrap = |detail| SecretError {
            operation: "delete",
            name: name.to_string(),
            detail,
        };
        match &self.backend {
            Backend::Native { open } => native_call(*open, name, native::delete).map_err(wrap),
            #[cfg(target_os = "macos")]
            Backend::SecurityCli => security_cli::delete(name).map_err(wrap),
            Backend::Memory(map) => {
                map.lock().unwrap().remove(name);
                Ok(())
            }
        }
    }
}

/// Open the platform store if this process has not, then make one call on
/// it, both on a fresh thread.
fn native_call<T: Send + 'static>(
    open: Open,
    name: &str,
    call: impl FnOnce(&str) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let name = name.to_string();
    on_fresh_thread(move || {
        open()?;
        call(&name)
    })
}

/// Run `work` on its own thread and wait for it.
fn on_fresh_thread<T: Send>(work: impl FnOnce() -> Result<T, String> + Send) -> Result<T, String> {
    std::thread::scope(|scope| {
        scope
            .spawn(work)
            .join()
            .map_err(|_| "the store call panicked".to_string())?
    })
}

/// Whether this binary carries a signing certificate — the condition under
/// which a keychain access-list entry for it survives a rebuild. An ad-hoc
/// signature (what the linker applies to every build) has no certificate and
/// fails the requirement.
#[cfg(target_os = "macos")]
pub fn identity_signed() -> bool {
    use security_framework::os::macos::code_signing::{Flags, SecCode, SecRequirement};
    let Ok(code) = SecCode::for_self(Flags::NONE) else {
        return false;
    };
    let Ok(requirement) = "certificate leaf[subject.CN] exists".parse::<SecRequirement>() else {
        return false;
    };
    code.check_validity(Flags::NONE, &requirement).is_ok()
}

mod native {
    use std::sync::{Mutex, PoisonError};

    use keyring_core::{Entry, Error};

    #[cfg(target_os = "macos")]
    pub const DESCRIPTION: &str = "the login keychain";
    #[cfg(target_os = "linux")]
    pub const DESCRIPTION: &str = "the Secret Service (login collection)";
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub const DESCRIPTION: &str = "no store on this platform";

    /// Whether the platform store is `keyring-core`'s default yet. Held for
    /// the length of an open, so concurrent first operations make one
    /// connection between them.
    static OPENED: Mutex<bool> = Mutex::new(false);

    /// Make the platform store `keyring-core`'s default if no earlier call
    /// has. A failure is not kept: the next operation tries again, so a
    /// store that comes up later serves a process that is already running.
    pub fn open() -> Result<(), String> {
        let mut opened = OPENED.lock().unwrap_or_else(PoisonError::into_inner);
        if !*opened {
            let store = platform_store()
                .map_err(|error| format!("{DESCRIPTION} could not be opened: {error}"))?;
            keyring_core::set_default_store(store);
            *opened = true;
        }
        Ok(())
    }

    /// Connect to this platform's store. On Linux this is the D-Bus dial.
    fn platform_store() -> Result<std::sync::Arc<keyring_core::CredentialStore>, String> {
        #[cfg(target_os = "macos")]
        {
            let store = apple_native_keyring_store::keychain::Store::new()
                .map_err(|error| error.to_string())?;
            Ok(store)
        }
        #[cfg(target_os = "linux")]
        {
            let store = zbus_secret_service_keyring_store::Store::new()
                .map_err(|error| error.to_string())?;
            Ok(store)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err("efference has no store backend for this platform".to_string())
        }
    }

    fn entry(name: &str) -> Result<Entry, String> {
        Entry::new(super::SERVICE, name).map_err(|error| error.to_string())
    }

    pub fn get(name: &str) -> Result<Option<String>, String> {
        match entry(name)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(Error::NoEntry) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    pub fn set(name: &str, value: &str) -> Result<(), String> {
        entry(name)?
            .set_password(value)
            .map_err(|error| error.to_string())
    }

    pub fn delete(name: &str) -> Result<(), String> {
        match entry(name)?.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
}

/// `security(1)` as the store: generic passwords in the login keychain.
#[cfg(target_os = "macos")]
mod security_cli {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    /// `security`'s exit status when a lookup matches nothing (`errSecItemNotFound`).
    const NOT_FOUND: i32 = 44;

    pub fn get(name: &str) -> Result<Option<String>, String> {
        let mut cmd = Command::new("security");
        cmd.args([
            "find-generic-password",
            "-s",
            super::SERVICE,
            "-a",
            name,
            "-w",
        ])
        .stdin(Stdio::null());
        let output = crate::spawn::std_output(&mut cmd)
            .map_err(|error| format!("could not run security: {error}"))?;
        if output.status.code() == Some(NOT_FOUND) {
            return Ok(None);
        }
        if !output.status.success() {
            return Err(failure(&output.stderr));
        }
        let mut value = String::from_utf8(output.stdout).map_err(|_| "value is not UTF-8")?;
        while value.ends_with('\n') {
            value.pop();
        }
        Ok(Some(value))
    }

    /// `security -i` reads commands from stdin, which is the one way to hand
    /// the tool a value without putting it on a command line. `-U` updates an
    /// existing item in place.
    pub fn set(name: &str, value: &str) -> Result<(), String> {
        let script = super::security_script(name, value);
        let mut child = crate::spawn::std(
            Command::new("security")
                .arg("-i")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped()),
        )
        .map_err(|error| format!("could not run security: {error}"))?;
        child
            .stdin
            .take()
            .expect("piped")
            .write_all(script.as_bytes())
            .map_err(|error| format!("could not write to security: {error}"))?;
        let output = child
            .wait_with_output()
            .map_err(|error| format!("could not wait for security: {error}"))?;
        // Interactive mode reports a failed command on stderr and exits 0, so
        // stderr is the verdict.
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() || !stderr.trim().is_empty() {
            return Err(failure(&output.stderr));
        }
        Ok(())
    }

    pub fn delete(name: &str) -> Result<(), String> {
        let output = crate::spawn::std_output(
            Command::new("security")
                .args(["delete-generic-password", "-s", super::SERVICE, "-a", name])
                .stdin(Stdio::null()),
        )
        .map_err(|error| format!("could not run security: {error}"))?;
        if output.status.success() || output.status.code() == Some(NOT_FOUND) {
            return Ok(());
        }
        Err(failure(&output.stderr))
    }

    fn failure(stderr: &[u8]) -> String {
        let text = String::from_utf8_lossy(stderr);
        let text = text.trim();
        if text.is_empty() {
            "security failed without a message".to_string()
        } else {
            text.to_string()
        }
    }
}

/// The `security -i` command line that stores `value` under `name`. Double
/// quotes delimit a word in that parser, with backslash escaping the quote and
/// itself; nothing else is interpreted.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn security_script(name: &str, value: &str) -> String {
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    format!(
        "add-generic-password -U -s {} -a {} -w {}\n",
        quote(SERVICE),
        quote(name),
        quote(value)
    )
}
