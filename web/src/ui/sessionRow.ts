// What a session-list row says about itself (docs/web-client.md), off the
// listing alone.

import type { ListedSession } from "../api/types.ts";

/**
 * Why archiving this session should be confirmed, or `null` when there is nothing
 * to lose by archiving it now — the same rule the TUI's switcher applies
 * (docs/tui.md): a session that is doing something, or that holds background
 * processes archiving would reap.
 */
export function archiveWarning(session: ListedSession): string | null {
  const doing =
    session.status === "running"
      ? "is mid-turn"
      : session.status === "gated"
        ? "is holding a call at a gate"
        : session.status === "asking"
          ? "is waiting on your answer to a question"
          : session.status === "stalled"
            ? "stalled and has not been answered"
            : session.status === "parked"
              ? "is paused on an expired cache, waiting for you"
              : session.status === "blocked"
                ? "is stopped mid-turn until someone acts"
                : null;
  const held =
    session.processes > 0
      ? `holds ${session.processes} background process${session.processes === 1 ? "" : "es"}`
      : null;
  if (doing === null && held === null) return null;
  return [doing, held].filter((part) => part !== null).join(" and ");
}
