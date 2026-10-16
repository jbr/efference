// Resolving a `p#<n>` handle to what it was (docs/web-client.md "An argument
// can carry a chip"; docs/process-monitor.md for the record itself).
//
// A handle is opaque in the transcript, so the chip that annotates it needs one
// fetch per handle — lazily, because most calls carry none. Answers are memoized
// for the page's lifetime: a record is the durable account of a spawn, and
// `404` is durable too (an id never minted into the background stays never
// minted, which is the normal answer for the recorded corpus). Only a transport
// failure is left uncached, so a chip that missed can resolve on a later render.
//
// The one staleness this accepts: a record fetched while the process runs keeps
// `running: true` in the cache after it exits. The chip's fact is the command,
// which does not change; the running/exited tone is a tint that catches up on
// reload.

import { useEffect, useState } from "react";
import { ApiError, fetchProcessRecord } from "../api/http.ts";
import type { ProcessRecord } from "../api/types.ts";

/** Resolved answers, `null` for "no such handle". */
const cache = new Map<number, ProcessRecord | null>();
/** One flight per id, so N chips on one handle make one request. */
const inflight = new Map<number, Promise<ProcessRecord | null>>();

/** The record for a handle, or null when there is none to have. Never throws:
 * a chip with no record is a chip that does not render. */
export function loadProcessRecord(id: number): Promise<ProcessRecord | null> {
  const hit = cache.get(id);
  if (hit !== undefined) return Promise.resolve(hit);
  const pending = inflight.get(id);
  if (pending !== undefined) return pending;
  const flight = fetchProcessRecord(id).then(
    (record) => {
      cache.set(id, record);
      inflight.delete(id);
      return record;
    },
    (error: unknown) => {
      inflight.delete(id);
      // Only the 404 is a fact about the session; anything else is this
      // request failing, and caching it would make the chip permanently blank.
      if (error instanceof ApiError && error.status === 404)
        cache.set(id, null);
      return null;
    },
  );
  inflight.set(id, flight);
  return flight;
}

/** The record for a handle as it becomes known. Null until it lands, and null
 * forever when there is none — the degraded chip renders the argument as the
 * plain text it already was. */
export function useProcessRecord(id: number): ProcessRecord | null {
  const [record, setRecord] = useState<ProcessRecord | null>(
    () => cache.get(id) ?? null,
  );
  useEffect(() => {
    let live = true;
    void loadProcessRecord(id).then((answer) => {
      if (live) setRecord(answer);
    });
    return () => {
      live = false;
    };
  }, [id]);
  return record;
}
