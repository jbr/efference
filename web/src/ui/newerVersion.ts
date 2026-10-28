// A newer release in the status line (docs/web-client.md): while the host's update
// check knows one (the `newer_version` topic, docs/update-check.md), the composer's
// readout row says so, with the command that installs it as the hover title.

import type { NewerVersion } from "../api/types.ts";

/** The line, or `null` while no newer release is known. */
export function newerVersionLine(newer: NewerVersion | null): string | null {
  return newer === null ? null : `efference ${newer.version} is available`;
}
