// Backward transcript windows and their boundary joins (docs/web-client.md).
import { fetchCallSeq, fetchLogBefore, fetchLogRange } from "../api/http.ts";
import type { LogEntry } from "../api/entry.ts";
import { assistantView } from "./assistant.ts";
import type { Session, Store } from "./store.ts";

export const HISTORY_PAGE = 80;

/** Older data is display history, not replay: never rewind topics, cursor, or
 * pre-commit output, and prefer entries already held when reads overlap. */
export function prependHistory(session: Session, entries: LogEntry[]): Session {
  const held = new Set(session.entries.map((entry) => entry.seq));
  const older = entries.filter((entry) => !held.has(entry.seq));
  if (older.length === 0) return session;
  return {
    ...session,
    entries: [...older, ...session.entries].sort((a, b) => a.seq - b.seq),
  };
}

/** Extend a cut back through the references needed by the transcript tree.
 * Fetching the intervening range keeps the pagination cursor contiguous: a
 * dereferenced call must not make a later page silently skip the gap to it. */
export async function coherentWindow(
  entries: LogEntry[],
  base: string,
): Promise<LogEntry[]> {
  let window = entries;
  const resolved = new Map<string, number>();
  while (window.length > 0) {
    const first = window[0].seq;
    let earliest = first;
    const calls = new Set(
      window.flatMap((entry) =>
        "Assistant" in entry.payload
          ? assistantView(entry.payload.Assistant).calls.map((call) => call.id)
          : [],
      ),
    );
    const missing = new Set<string>();
    for (const entry of window) {
      const p = entry.payload;
      const id =
        "ToolResult" in p
          ? p.ToolResult.call_id
          : "Spot" in p
            ? p.Spot.call_id
            : "Telemetry" in p && "Tool" in p.Telemetry
              ? p.Telemetry.Tool.call_id
              : null;
      if (id !== null && !calls.has(id)) missing.add(id);
      if ("ResultVerdict" in p)
        earliest = Math.min(earliest, p.ResultVerdict.for_result);
    }
    await Promise.all(
      [...missing].map(async (id) => {
        if (!resolved.has(id))
          resolved.set(id, (await fetchCallSeq(id, base)).seq);
      }),
    );
    for (const id of missing) earliest = Math.min(earliest, resolved.get(id)!);
    if (earliest >= first) return window;
    const extra = await fetchLogRange(earliest, first, base);
    if (extra.length === 0 || extra[0].seq >= first)
      throw new Error("Earlier transcript references could not be loaded.");
    window = [...extra, ...window];
  }
  return window;
}

export interface HistoryStatus {
  loading: boolean;
  done: boolean;
  error: string | null;
}

/** Owned by the warm store, not by a route's DOM. In-flight reads retain their
 * source session; a changed window invalidates their cursor before merging. */
export class HistoryLoader {
  private status: HistoryStatus = { loading: false, done: false, error: null };
  private listeners = new Set<() => void>();
  constructor(
    private store: Store,
    private base: string,
  ) {
    let first = store.snapshot().entries[0]?.seq;
    store.subscribe(() => {
      const next = store.snapshot().entries[0]?.seq;
      if (first !== undefined && next !== undefined && next > first) {
        this.set({ loading: this.status.loading, done: false, error: null });
      }
      first = next;
    });
  }
  snapshot = () => this.status;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
  private set(status: HistoryStatus) {
    this.status = status;
    for (const listener of this.listeners) listener();
  }
  async load(beforeMerge?: () => void): Promise<void> {
    const before = this.store.snapshot().entries[0]?.seq;
    if (before === undefined || this.status.loading || this.status.done) return;
    this.set({ loading: true, done: false, error: null });
    try {
      const page = await fetchLogBefore(before, HISTORY_PAGE, this.base);
      const entries = await coherentWindow(page, this.base);
      if (this.store.snapshot().entries[0]?.seq !== before) {
        this.set({ loading: false, done: false, error: null });
        return;
      }
      beforeMerge?.();
      this.store.update((session) => prependHistory(session, entries));
      this.set({
        loading: false,
        done: page.length < HISTORY_PAGE,
        error: null,
      });
    } catch (error) {
      this.set({
        loading: false,
        done: false,
        error:
          error instanceof Error
            ? error.message
            : "Could not load earlier entries.",
      });
    }
  }
}
const loaders = new WeakMap<Store, HistoryLoader>();
export function acquireHistory(store: Store, base: string): HistoryLoader {
  let loader = loaders.get(store);
  if (!loader) {
    loader = new HistoryLoader(store, base);
    loaders.set(store, loader);
  }
  return loader;
}
