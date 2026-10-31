import { sessionSource } from "../api/source.ts";
import type { InterruptOrigin } from "../api/types.ts";

/** Client-asserted origin of a recorded interrupt (docs/control-api.md). */
export function interruptFrom(origin: InterruptOrigin): string {
  const source = origin.source ?? undefined;
  const control = origin.control ?? undefined;
  if (source === "web") {
    if (control === "stop-button") return "the web stop button";
    if (control === "status-bar-stop") return "the web status-bar stop button";
    return control ? `web (${control})` : "web";
  }
  if (source === undefined) {
    if (control === "esc") return "TUI Esc";
    if (control === "control-cli") return "the command line";
    return control
      ? `the command line or TUI (${control})`
      : "the command line or TUI";
  }
  const session =
    sessionSource(source)?.id ?? /^agent\[(.*)\]$/.exec(source)?.[1];
  const mcp = /^mcp\[(.*)\]$/.exec(source);
  const sender =
    session !== undefined
      ? `session ${session}`
      : mcp
        ? `MCP client ${mcp[1]}`
        : source;
  return control ? `${sender} (${control})` : sender;
}
