// Which files a call names, for the file viewer (docs/web-client.md;
// docs/web-client.md). Session-layer derivation over the log the store
// already holds — the /files endpoint contributes nothing and knows nothing
// about the session.
//
// The viewer is within-cwd only, categorically (docs/web-client.md
// viewer.md): an absolute path is out of the surface's scope, not merely
// unresolvable, so it yields nothing here and the call's own log diff stays
// its only surface. A `..`-leading path would escape the root the same way
// and is declined for the same reason.

import type { CallId } from "../api/types.ts";
import type { LogEntry } from "../api/entry.ts";
import { assistantView } from "./assistant.ts";
import { anchorFromResult, type DiffAnchor } from "./editAnchor.ts";

const EDIT_TOOLS = new Set(["write", "edit", "undo_edit", "retarget_edit"]);

/** A path the /files surface can serve: relative, inside the root. */
export function viewablePath(path: string): string | null {
  if (path.startsWith("/") || path === "") return null;
  const segments = path.split("/").filter((s) => s !== "" && s !== ".");
  if (segments.length === 0 || segments[0] === "..") return null;
  return segments.join("/");
}

const str = (input: unknown, key: string): string | null => {
  if (typeof input !== "object" || input === null) return null;
  const value = (input as Record<string, unknown>)[key];
  return typeof value === "string" ? value : null;
};

/** The file a call changes on disk, when it names one the viewer can serve.
 * Edits and writes only — this is the follow signal, and a bare `read` does
 * not move the view (docs/web-client.md). */
export function editedPath(name: string, input: unknown): string | null {
  if (!EDIT_TOOLS.has(name)) return null;
  const path = str(input, name === "write" ? "path" : "file_path");
  return path === null ? null : viewablePath(path);
}

/** Every viewable file a call names — edits plus reads — for the `open ⤢`
 * affordance on the call. */
export function openablePaths(name: string, input: unknown): string[] {
  const edited = editedPath(name, input);
  if (edited !== null) return [edited];
  if (name === "read" && typeof input === "object" && input !== null) {
    const paths = (input as Record<string, unknown>).paths;
    if (Array.isArray(paths)) {
      return paths
        .filter((p): p is string => typeof p === "string")
        .map(viewablePath)
        .filter((p): p is string => p !== null);
    }
  }
  return [];
}

export interface EditEvent {
  /** The `ToolResult` entry's seq — a strictly increasing identity, so an
   * effect keyed on it fires once per landed edit. */
  seq: number;
  path: string;
  /** Where in the file the edit landed (session/editAnchor.ts), when its
   * result carried a diff — what makes follow land *on* the edit rather than
   * at the top of the file. */
  anchor: DiffAnchor | null;
}

/** The newest committed, non-errored edit or write naming a viewable path —
 * what follow-the-agent follows. Errored calls are skipped because they
 * changed nothing on disk. */
export function lastEdit(entries: LogEntry[]): EditEvent | null {
  const calls = new Map<CallId, { name: string; input: unknown }>();
  let last: EditEvent | null = null;
  for (const entry of entries) {
    const p = entry.payload;
    if ("Assistant" in p) {
      for (const call of assistantView(p.Assistant).calls) {
        calls.set(call.id, { name: call.name, input: call.input });
      }
      continue;
    }
    if (!("ToolResult" in p) || p.ToolResult.raw.is_error) continue;
    const call = calls.get(p.ToolResult.call_id);
    if (call === undefined) continue;
    const path = editedPath(call.name, call.input);
    if (path !== null) {
      last = {
        seq: entry.seq,
        path,
        anchor: anchorFromResult(p.ToolResult.raw.content),
      };
    }
  }
  return last;
}

/** What the session has done to a file — the tree's annotations
 * (docs/web-client.md "Navigating the working directory"). */
export interface FileActivity {
  /** A `read` named it. */
  read: boolean;
  /** An edit or write changed it. */
  edited: boolean;
  /** The newest entry seq naming it — recency, for "the working set". */
  seq: number;
}

/** Which files this session has touched, derived client-side from the log the
 * store already holds: the `/files` endpoint contributes nothing to this and
 * knows nothing about the session. Errored calls count for nothing — a read
 * that failed read nothing, an edit that failed changed nothing. */
export function fileActivity(entries: LogEntry[]): Map<string, FileActivity> {
  const calls = new Map<CallId, { name: string; input: unknown }>();
  const activity = new Map<string, FileActivity>();
  const mark = (path: string, seq: number, edited: boolean) => {
    const at = activity.get(path);
    activity.set(path, {
      read: (at?.read ?? false) || !edited,
      edited: (at?.edited ?? false) || edited,
      seq,
    });
  };
  for (const entry of entries) {
    const p = entry.payload;
    if ("Assistant" in p) {
      for (const call of assistantView(p.Assistant).calls) {
        calls.set(call.id, { name: call.name, input: call.input });
      }
      continue;
    }
    if (!("ToolResult" in p) || p.ToolResult.raw.is_error) continue;
    const call = calls.get(p.ToolResult.call_id);
    if (call === undefined) continue;
    const edited = editedPath(call.name, call.input);
    if (edited !== null) {
      mark(edited, entry.seq, true);
      continue;
    }
    for (const path of openablePaths(call.name, call.input)) {
      mark(path, entry.seq, false);
    }
  }
  return activity;
}

/** The anchor for one call's edit, for the `open ⤢` on a call that is not the
 * newest — the historical case. `null` when the call has not returned, errored,
 * or is not an edit tool, all of which mean there is nothing to point at. */
export function anchorForCall(
  entries: LogEntry[],
  callId: CallId | null,
): DiffAnchor | null {
  if (callId === null) return null;
  for (const entry of entries) {
    const p = entry.payload;
    if (!("ToolResult" in p) || p.ToolResult.call_id !== callId) continue;
    if (p.ToolResult.raw.is_error) return null;
    return anchorFromResult(p.ToolResult.raw.content);
  }
  return null;
}
