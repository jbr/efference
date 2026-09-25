// Dev-only persistence for the stream scrubber's tapes (session/record.ts):
// a reload keeps what the last run recorded, so a mid-stream state can be
// revisited without waiting for the model to produce it again.
//
// IndexedDB rather than localStorage: a tape's base is the whole seeded
// session, tool results included, which outgrows localStorage's ~5 MB. Each
// recording is its own record, keyed by session and start time, and a
// session keeps its newest KEEP — a reload starts a new recording rather than
// overwriting the one worth going back to — and keeps its largest LARGEST as
// well, because a run of short recordings (a session driven turn by turn)
// would otherwise push out the long one a design pass was stepping through.

import type { Frame } from "../api/types.ts";
import type { Session } from "./store.ts";

const DB = "efference-scrub";
const STORE = "tapes";
const KEEP = 5;
const LARGEST = 3;

export interface SavedTape {
  key: string;
  session: string;
  startedAt: number;
  base: Session;
  tape: Frame[];
}

function open(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DB, 1);
    request.onupgradeneeded = () => {
      const store = request.result.createObjectStore(STORE, { keyPath: "key" });
      store.createIndex("session", "session");
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

function done<T>(request: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

/** The session's saved tapes, newest first, without their frames. */
export async function listTapes(
  session: string,
): Promise<{ key: string; startedAt: number; frames: number }[]> {
  const db = await open();
  const all = await done(
    db
      .transaction(STORE)
      .objectStore(STORE)
      .index("session")
      .getAll(session) as IDBRequest<SavedTape[]>,
  );
  return all
    .sort((a, b) => b.startedAt - a.startedAt)
    .map(({ key, startedAt, tape }) => ({
      key,
      startedAt,
      frames: tape.length,
    }));
}

export async function loadTape(key: string): Promise<SavedTape | undefined> {
  const db = await open();
  return done(
    db.transaction(STORE).objectStore(STORE).get(key) as IDBRequest<
      SavedTape | undefined
    >,
  );
}

/** Save a tape, then prune the session to its newest KEEP and its largest
 * LARGEST. */
export async function saveTape(tape: SavedTape): Promise<void> {
  const db = await open();
  await done(db.transaction(STORE, "readwrite").objectStore(STORE).put(tape));
  const saved = await listTapes(tape.session);
  const keep = new Set([
    ...saved.slice(0, KEEP).map((t) => t.key),
    ...[...saved]
      .sort((a, b) => b.frames - a.frames)
      .slice(0, LARGEST)
      .map((t) => t.key),
  ]);
  const store = db.transaction(STORE, "readwrite").objectStore(STORE);
  for (const old of saved) if (!keep.has(old.key)) store.delete(old.key);
}
