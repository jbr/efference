// The tool definition behind a call, for the inspector's args tab
// (docs/web-client.md "The inspector"; docs/control-api.md for the route).
//
// Fetched lazily, one name at a time, and memoized for the page's lifetime: the
// catalog is snapshotted before the turn loop starts and is constant for the
// session, so a definition once read is the definition every call in the tab was
// made against. `404` is durable too — an unknown name stays unknown, and so does
// *every* name in a session read with no daemon behind it, which is the offline
// case the tab degrades into rather than an error to surface. Only a transport
// failure is left uncached, so a drawer reopened later can still resolve.
//
// Mirrors processRecord.ts, which draws the same distinction for `p#n` handles.

import { useEffect, useState } from "react";
import { ApiError, fetchToolEntry } from "../api/http.ts";
import type { ToolEntry } from "../api/types.ts";

/** Resolved answers, `null` for "no definition to have". */
const cache = new Map<string, ToolEntry | null>();
/** One flight per name, so several drawers on one tool make one request. */
const inflight = new Map<string, Promise<ToolEntry | null>>();

/** The definition for a wire tool name, or null when there is none. Never
 * throws: no definition is a tab that shows only what the call sent. */
export function loadToolEntry(name: string): Promise<ToolEntry | null> {
  const hit = cache.get(name);
  if (hit !== undefined) return Promise.resolve(hit);
  const pending = inflight.get(name);
  if (pending !== undefined) return pending;
  const flight = fetchToolEntry(name).then(
    (entry) => {
      cache.set(name, entry);
      inflight.delete(name);
      return entry;
    },
    (error: unknown) => {
      inflight.delete(name);
      // Only the 404 is a fact about the session; anything else is this
      // request failing, and caching it would strand the tab unglossed.
      if (error instanceof ApiError && error.status === 404)
        cache.set(name, null);
      return null;
    },
  );
  inflight.set(name, flight);
  return flight;
}

/** The definition as it becomes known. Null until it lands, and null forever
 * when there is none — no spinner and no error row: the tab renders the sent
 * arguments the whole time and gains the gloss when it arrives. */
export function useToolEntry(name: string | null): ToolEntry | null {
  // Keyed by the name it was read for, so a drawer that switches to another
  // call discards the previous definition at render time rather than flashing
  // one tool's prose under another tool's arguments.
  const [held, setHeld] = useState<{ name: string; entry: ToolEntry } | null>(
    () => {
      if (name === null) return null;
      const hit = cache.get(name);
      return hit === undefined || hit === null ? null : { name, entry: hit };
    },
  );
  useEffect(() => {
    if (name === null) return;
    let live = true;
    void loadToolEntry(name).then((answer) => {
      if (live && answer !== null) setHeld({ name, entry: answer });
    });
    return () => {
      live = false;
    };
  }, [name]);
  return held !== null && held.name === name ? held.entry : null;
}
