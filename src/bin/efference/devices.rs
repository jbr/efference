//! `efference devices` (docs/web-bind.md): the devices enrolled at this machine's web
//! bind — list them, enroll a script, revoke one. Over the host's socket, where
//! possession of the host is the enrollment authority; none of it is served on TCP.

use super::*;
use efference::devices::Device;

#[cfg(test)]
mod test;

/// `efference devices <verb>`; with none, the listing.
#[derive(Subcommand)]
pub(crate) enum DevicesCommand {
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
    // The device routes live at the socket root: the first live host's.
    let host = live_host().ok_or("no host is running; start one with `efference --host`")?;
    let client = ControlClient::new(host);
    match command {
        None => print!("{}", listing(&client.devices().await?, now_secs())),
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

/// The listing as `efference devices` prints it: a header and one row per device,
/// times as ages, the label last so it needs no padding.
fn listing(devices: &[Device], now: i64) -> String {
    if devices.is_empty() {
        return "No devices are enrolled. A browser enrolls by signing in at a link the \
                host prints; `efference devices add <label>` enrolls a script.\n"
            .to_string();
    }
    let ago = |at: i64| efference::tui::resume_list::age(millis(now), millis(at));
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

/// Unix seconds as the milliseconds [`efference::tui::resume_list::age`] takes.
fn millis(secs: i64) -> u64 {
    u64::try_from(secs).unwrap_or(0).saturating_mul(1000)
}
