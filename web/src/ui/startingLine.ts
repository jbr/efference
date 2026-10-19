// What a starting session's preparation is doing, in the composer readout
// (docs/web-client.md).

import type { Starting } from "../api/types.ts";

export function startingLine(starting: Starting): string {
  if (starting.authorizing !== null)
    return `Starting — waiting for ${starting.authorizing} to be authorized in a browser on the host`;
  return starting.stage === "environment"
    ? "Starting — running the env hook"
    : "Starting — loading the config and mounting MCP servers";
}
