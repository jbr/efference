use crate::config::user_config::InferenceConfig;
use crate::platform::secrets::testing::ScriptStore;
use crate::platform::secrets::{SecretError, Secrets};

/// With a `[secrets]` table the store is its commands, and nothing else is chosen;
/// with none it is the platform's.
#[test]
fn the_table_selects_the_commands() {
    let store = ScriptStore::new("select", "absent_status = 3");
    let secrets = store.secrets();
    assert!(secrets.runs_commands());
    assert_eq!(secrets.describe(), "the `[secrets]` store");
    assert_eq!(secrets.open(), Ok(()));

    let empty = std::env::temp_dir().join(format!("efference-no-config-{}", std::process::id()));
    let platform = Secrets::configured(InferenceConfig::secret_commands(&empty).unwrap());
    assert!(!platform.runs_commands());
}

/// A value goes in on stdin and comes back on stdout; a second `set` replaces it, and
/// `delete` removes it. The value is never an argument: `set` was given the item's
/// name and nothing else.
#[test]
fn a_value_round_trips_through_the_commands() {
    let store = ScriptStore::new("round-trip", "absent_status = 3");
    let secrets = store.secrets();
    assert_eq!(secrets.get("oauth:codex").unwrap(), None);
    secrets.set("oauth:codex", "sk-first").unwrap();
    assert_eq!(store.item("oauth:codex").as_deref(), Some("sk-first"));
    assert_eq!(
        secrets.get("oauth:codex").unwrap().as_deref(),
        Some("sk-first")
    );
    secrets.set("oauth:codex", "sk-second").unwrap();
    assert_eq!(
        secrets.get("oauth:codex").unwrap().as_deref(),
        Some("sk-second")
    );
    secrets.delete("oauth:codex").unwrap();
    assert_eq!(secrets.get("oauth:codex").unwrap(), None);
    // Deleting what is absent is not an error: `delete` exiting `absent_status` is.
    secrets.delete("oauth:codex").unwrap();

    let items = store.items().display().to_string();
    assert_eq!(
        store.set_arguments(),
        format!("{items} oauth:codex\n{items} oauth:codex\n")
    );
}

/// The value is written exactly, and one trailing newline of what `get` prints is
/// trimmed: a tool that ends its output with a newline returns a value with spaces
/// or a newline of its own unchanged.
#[test]
fn a_value_with_spaces_and_a_trailing_newline_comes_back_unchanged() {
    let store = ScriptStore::new("newline", "absent_status = 3");
    let secrets = store.secrets();
    let value = "  two words, \"quoted\" $HOME `x`\n";
    secrets.set("mcp:linear", value).unwrap();
    assert_eq!(store.item("mcp:linear").as_deref(), Some(value));
    assert_eq!(secrets.get("mcp:linear").unwrap().as_deref(), Some(value));
}

/// Only one trailing newline is trimmed, and output with none is the value as printed.
#[test]
fn get_trims_one_trailing_newline_and_no_more() {
    let store = ScriptStore::new("trim", "");
    store.configure(&format!(
        "get = [\"printf\", \"%s\\n\\n\", \"{{name}}\"]\nset = {}",
        store.argv("set.sh")
    ));
    assert_eq!(
        store.secrets().get("root").unwrap().as_deref(),
        Some("root\n")
    );
    store.configure(&format!(
        "get = [\"printf\", \"%s\", \"{{name}}\"]\nset = {}",
        store.argv("set.sh")
    ));
    assert_eq!(
        store.secrets().get("root").unwrap().as_deref(),
        Some("root")
    );
}

/// Absence is the declared status and nothing else: another failure is an error
/// carrying the command's stderr, and with no `absent_status` a missing item is a
/// failure too.
#[test]
fn absent_status_is_absence_and_any_other_failure_is_an_error() {
    let store = ScriptStore::new("absent", "absent_status = 3");
    assert_eq!(store.secrets().get("inference:work").unwrap(), None);

    store.script("sealed.sh", "echo 'vault is sealed' >&2; exit 2");
    store.configure(&format!(
        "get = {}\nset = {}\nabsent_status = 3",
        store.argv("sealed.sh"),
        store.argv("set.sh")
    ));
    let error = store.secrets().get("inference:work").unwrap_err();
    assert_eq!(
        error,
        SecretError {
            operation: "read",
            name: "inference:work".into(),
            detail: "`sh` exited with status 2: vault is sealed".into(),
        }
    );

    let undeclared = ScriptStore::new("undeclared", "");
    let error = undeclared.secrets().get("inference:work").unwrap_err();
    assert_eq!(
        error.to_string(),
        "could not read secret `inference:work`: `sh` exited with status 3 and printed \
         nothing on stderr"
    );
}

/// A failing `set` is an error naming the item, and a program that cannot be run says
/// which.
#[test]
fn a_failed_write_and_a_missing_program_are_errors() {
    let store = ScriptStore::new("write", "");
    store.script("full.sh", "cat > /dev/null; echo 'disk full' >&2; exit 1");
    store.configure(&format!(
        "get = {}\nset = {}",
        store.argv("get.sh"),
        store.argv("full.sh")
    ));
    assert_eq!(
        store
            .secrets()
            .set("root", "value")
            .unwrap_err()
            .to_string(),
        "could not write secret `root`: `sh` exited with status 1: disk full"
    );

    store.configure(
        "get = [\"efference-no-such-program\", \"{name}\"]\nset = [\"true\", \"{name}\"]",
    );
    let error = store.secrets().get("root").unwrap_err().to_string();
    assert!(
        error
            .starts_with("could not read secret `root`: could not run `efference-no-such-program`"),
        "{error}"
    );
}

/// Without a `delete` command, a removal fails, naming the item to remove by hand.
#[test]
fn delete_without_a_command_names_the_item_to_remove() {
    let store = ScriptStore::new("no-delete", "");
    store.configure(&format!(
        "get = {}\nset = {}",
        store.argv("get.sh"),
        store.argv("set.sh")
    ));
    assert_eq!(
        store
            .secrets()
            .delete("mcp:linear")
            .unwrap_err()
            .to_string(),
        "could not delete secret `mcp:linear`: [secrets] has no `delete` command; remove \
         the item by hand"
    );
}

/// The parser refuses a table that cannot address an item: a missing `get` or `set`,
/// an empty command, a command with no `{name}`, and an absent status of 0, which is
/// success.
#[test]
fn a_table_that_cannot_address_an_item_is_refused() {
    let refused = |table: &str| {
        InferenceConfig::from_toml(&format!("[secrets]\n{table}"))
            .err()
            .unwrap_or_else(|| panic!("accepted: {table}"))
            .to_string()
    };
    assert!(refused("get = [\"pass\", \"show\", \"{name}\"]").contains("set"));
    assert!(
        refused("get = []\nset = [\"x\", \"{name}\"]")
            .contains("[secrets] get = [] names no program")
    );
    assert!(
        refused("get = [\"cat\", \"/secret\"]\nset = [\"x\", \"{name}\"]")
            .contains("[secrets] get has no {name} argument")
    );
    assert!(
        refused("get = [\"x\", \"{name}\"]\nset = [\"x\", \"{name}\"]\ndelete = [\"rm\"]")
            .contains("[secrets] delete has no {name} argument")
    );
    assert!(
        refused("get = [\"x\", \"{name}\"]\nset = [\"x\", \"{name}\"]\nabsent_status = 0")
            .contains("nonzero")
    );
    assert!(
        refused("get = [\"x\", \"{name}\"]\nset = [\"x\", \"{name}\"]\ntimeout = 3")
            .contains("timeout")
    );
}
