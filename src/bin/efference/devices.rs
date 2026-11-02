//! `efference devices` (docs/web-bind.md): the devices enrolled at this machine's web
//! bind and the browsers asking to join them — list both, let a browser in by its
//! code, enroll a script, revoke one. Over the host's socket, where possession of the
//! host is the enrollment authority; none of it is served on TCP.

use super::*;
use efference::platform::devices::Device;
use efference::platform::sign_ins::Pending;
use std::io::{BufRead, Write};

#[cfg(test)]
mod test;

/// `efference devices <verb>`; with none, the listing.
#[derive(Subcommand)]
pub(crate) enum DevicesCommand {
    /// Let a browser in: type the code it shows.
    ///
    /// A browser that opens the web address without a sign-in link shows a short
    /// code, like KQ7-M4X, for a minute and a half. Approving the code here lets
    /// that browser collect a token of its own, as a new device. With no code,
    /// asks for it on this terminal. Prints what was approved: the name the browser
    /// gave and how long ago it asked.
    Approve {
        /// The code the browser shows; either case, the dash optional. Words are
        /// joined, so `kq7 m4x` is the same code.
        code: Vec<String>,
    },
    /// Enroll a script: print a token for its `Authorization: Bearer` header.
    ///
    /// The token is printed once, on stdout alone, and no later command shows it
    /// again; `efference devices revoke` ends it.
    Add {
        /// What `efference devices` calls it.
        label: String,
    },
    /// Revoke one device, by its id or the first characters of it.
    ///
    /// Its token is refused from the next request on, by every host on this
    /// machine. A browser that is signed out this way signs in again as a new
    /// device.
    Revoke {
        /// The id as `efference devices` lists it, or more of it when that is
        /// ambiguous.
        id: String,
    },
}

pub(crate) async fn run(command: Option<DevicesCommand>) -> CliResult {
    // The device routes live at the socket root: the user's host's.
    let host = live_host().ok_or("no host is running; start one with `efference --host`")?;
    let client = ControlClient::new(host);
    match command {
        None => {
            let devices = client.devices().await?;
            let pending = client.pending_sign_ins().await?;
            print!("{}", listing(&devices, &pending, now_secs()));
        }
        Some(DevicesCommand::Approve { code }) => {
            let code = match code.concat() {
                typed if typed.is_empty() => ask_for_code()?,
                typed => typed,
            };
            let approved = client.approve_sign_in(&code).await??;
            eprintln!(
                "Approved {} (asked {}); it signs in at its next check, within a second.",
                approved.label,
                seconds_ago(now_secs(), approved.created_at)
            );
        }
        Some(DevicesCommand::Add { label }) => {
            let enrolled = client.add_device(&label).await?;
            println!("{}", enrolled.token);
            let short = enrolled.device.id.short();
            eprintln!(
                "Enrolled script {short} ({}). The token above is not shown again; \
                 `efference devices revoke {short}` ends it.",
                enrolled.device.label
            );
        }
        Some(DevicesCommand::Revoke { id }) => {
            let devices = client.devices().await?;
            let device = matching(&devices, &id)?;
            client.revoke_device(&device.id).await?;
            eprintln!(
                "Revoked {} ({}): its token is refused from the next request on.",
                device.id.short(),
                device.label
            );
        }
    }
    Ok(())
}

/// The code to approve, asked for on the controlling terminal — `/dev/tty`, so that
/// neither a redirected stdin nor a redirected stdout stands in for the person at the
/// host. With no terminal, the error names the argument form.
fn ask_for_code() -> Result<String, CliError> {
    let tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map_err(
            |_| "no terminal to ask for the code on; pass it: `efference devices approve <code>`",
        )?;
    read_code(std::io::BufReader::new(&tty), &tty)
}

/// Prompt on `output` and read one line of `input` as the code.
fn read_code(mut input: impl BufRead, mut output: impl Write) -> Result<String, CliError> {
    write!(output, "Code the browser shows: ")?;
    output.flush()?;
    let mut line = String::new();
    input.read_line(&mut line)?;
    match line.trim() {
        "" => Err("no code given; the browser shows one, like KQ7-M4X".into()),
        code => Ok(code.to_string()),
    }
}

/// The listing as `efference devices` prints it: the devices — a header and one row
/// each, times as ages, the label last so it needs no padding — then the sign-ins
/// waiting for approval, by label and age. Never a code: approving is typing what the
/// browser shows.
fn listing(devices: &[Device], pending: &[Pending], now: i64) -> String {
    let mut out = device_listing(devices, now);
    if !pending.is_empty() {
        out.push_str(
            "\nWaiting to sign in; `efference devices approve` takes the code each one \
             shows:\n",
        );
        let rows: Vec<[String; 2]> = pending
            .iter()
            .map(|pending| [seconds_ago(now, pending.created_at), pending.label.clone()])
            .collect();
        let width = rows
            .iter()
            .map(|row| row[0].len())
            .chain(["asked".len()])
            .max()
            .unwrap_or(0);
        out.push_str(&format!("{:width$}  label\n", "asked"));
        for [asked, label] in rows {
            out.push_str(&format!("{asked:width$}  {label}\n"));
        }
    }
    out
}

/// A pending sign-in's age, in seconds: it lives a minute and a half, so the coarser
/// ages the device rows use would read "just now" for all of it.
fn seconds_ago(now: i64, at: i64) -> String {
    format!("{}s ago", (now - at).max(0))
}

/// The device half of [`listing`].
fn device_listing(devices: &[Device], now: i64) -> String {
    if devices.is_empty() {
        return "No devices are enrolled. A browser enrolls by opening a sign-in link the \
                host prints, or by showing a code `efference devices approve` takes; \
                `efference devices add <label>` enrolls a script.\n"
            .to_string();
    }
    let ago = |at: i64| efference::clients::tui::resume_list::age(millis(now), millis(at));
    let mut rows = vec![[
        "id".to_string(),
        "kind".to_string(),
        "joined".to_string(),
        "created".to_string(),
        "last seen".to_string(),
        "label".to_string(),
    ]];
    rows.extend(devices.iter().map(|device| {
        [
            device.id.short(),
            device.kind.as_str().to_string(),
            device.joined.as_str().to_string(),
            ago(device.created_at),
            device.last_seen_at.map_or_else(|| "never".to_string(), ago),
            device.label.clone(),
        ]
    }));
    let widths: Vec<usize> = (0..5)
        .map(|column| rows.iter().map(|row| row[column].len()).max().unwrap_or(0))
        .collect();
    let mut out = String::new();
    for row in &rows {
        for (cell, width) in row.iter().zip(&widths) {
            out.push_str(&format!("{cell:width$}  "));
        }
        out.push_str(&row[5]);
        out.push('\n');
    }
    out
}

/// The one device whose id starts with `prefix`, either case. None and several are
/// both errors, naming what there is to choose from.
fn matching<'a>(devices: &'a [Device], prefix: &str) -> Result<&'a Device, CliError> {
    let prefix = prefix.trim().to_ascii_lowercase();
    if prefix.is_empty() {
        return Err("name the device by its id, or its first characters, as \
                    `efference devices` lists it"
            .into());
    }
    let found: Vec<&Device> = devices
        .iter()
        .filter(|device| device.id.hex().starts_with(&prefix))
        .collect();
    let named = |devices: &[&Device], id: fn(&Device) -> String| {
        devices
            .iter()
            .map(|device| format!("{} ({})", id(device), device.label))
            .collect::<Vec<_>>()
            .join(", ")
    };
    match found.as_slice() {
        [device] => Ok(device),
        [] if devices.is_empty() => Err("no devices are enrolled".into()),
        [] => Err(format!(
            "no enrolled device's id starts with {prefix:?}; enrolled: {}",
            named(&devices.iter().collect::<Vec<_>>(), |d| d.id.short())
        )
        .into()),
        several => Err(format!(
            "{prefix:?} starts {} devices' ids: {}; give more of the id",
            several.len(),
            named(several, |d| d.id.hex())
        )
        .into()),
    }
}

/// Unix seconds now, the unit the table records.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Unix seconds as the milliseconds [`efference::clients::tui::resume_list::age`] takes.
fn millis(secs: i64) -> u64 {
    u64::try_from(secs).unwrap_or(0).saturating_mul(1000)
}
