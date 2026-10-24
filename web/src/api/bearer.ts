// The page's credential (docs/web-bind.md): the bearer the mint trades a
// current code for, kept in IndexedDB and sent by `apiFetch` (api/http.ts) as
// `Authorization: Bearer` on every request. IndexedDB is origin-scoped, port
// included, so no other service on the same host can read it. It is also
// readable from the service worker, where `localStorage` is not: the worker's
// push-subscription repair (`resubscribe` in public/sw.js) authenticates with
// the same value, read under the same three names, and pwa.test.ts pins that
// the two spellings agree.
//
// One in-memory copy, read from storage once at boot before the first request
// (main.tsx), so attaching the header stays synchronous. Storage that is
// missing or throws (some private modes) leaves the memory copy working: the
// page stays signed in until it reloads.

/** The database, object store, and key the bearer is kept under. The worker
 * spells the same three (public/sw.js). */
export const BEARER_DB = "efference-auth";
export const BEARER_STORE = "credentials";
export const BEARER_KEY = "bearer";

/** How long boot waits for storage before going on without it. IndexedDB
 * answers in milliseconds, but an open that never answers would otherwise
 * leave the page unrendered; past this the page boots signed out, which the
 * sign-in view repairs. */
export const LOAD_WAIT_MS = 3000;

/** Where the bearer persists: IndexedDB in the page, a stand-in in tests. */
export interface BearerBackend {
  read(): Promise<string | null>;
  write(value: string): Promise<void>;
  erase(): Promise<void>;
}

export interface BearerStore {
  /** The held bearer, or `null` — what `apiFetch` reads on every request. */
  held(): string | null;
  /** Read storage into memory. Resolves once storage answers, fails, or
   * `ms` passes, and never rejects. */
  load(ms?: number): Promise<void>;
  /** Hold `value` and persist it. */
  store(value: string): Promise<void>;
  /** Forget the held bearer and erase the stored one. */
  clear(): Promise<void>;
}

/** A store over `backend`. Writes reach the backend in call order, so a clear
 * still in flight when a fresh sign-in stores its bearer cannot land after it;
 * and a load that answers after either leaves memory alone, since memory is
 * then the newer of the two. */
export function bearerStore(backend: BearerBackend): BearerStore {
  let held: string | null = null;
  let changed = false;
  let writes: Promise<void> = Promise.resolve();
  const persist = (write: () => Promise<void>) => {
    writes = writes.then(write).catch(() => undefined);
    return writes;
  };
  return {
    held: () => held,
    async load(ms = LOAD_WAIT_MS) {
      let timer: ReturnType<typeof setTimeout> | undefined;
      const value = await Promise.race([
        backend.read().catch(() => null),
        new Promise<null>((resolve) => {
          timer = setTimeout(() => resolve(null), ms);
        }),
      ]);
      clearTimeout(timer);
      if (!changed) held = value;
    },
    store(value) {
      held = value;
      changed = true;
      return persist(() => backend.write(value));
    },
    clear() {
      held = null;
      changed = true;
      return persist(() => backend.erase());
    },
  };
}

/** The browser's IndexedDB under the names above. `factory` is read inside
 * each call, so a context where touching `indexedDB` throws fails that call
 * rather than this module's load; and opening the database per call holds no
 * connection between the page's three uses of it: boot, a sign-in, a `401`. */
export function indexedDbBackend(
  factory: () => IDBFactory | undefined = () => globalThis.indexedDB,
): BearerBackend {
  async function run<T>(
    mode: IDBTransactionMode,
    act: (store: IDBObjectStore) => IDBRequest<T>,
  ): Promise<T> {
    const idb = factory();
    if (!idb) throw new Error("IndexedDB is unavailable");
    const db = await open(idb);
    try {
      return await new Promise<T>((resolve, reject) => {
        const transaction = db.transaction(BEARER_STORE, mode);
        const request = act(transaction.objectStore(BEARER_STORE));
        // A write has landed when its transaction completes, not when its
        // request succeeds.
        transaction.oncomplete = () => resolve(request.result);
        transaction.onerror = () => reject(transaction.error);
        transaction.onabort = () => reject(transaction.error);
      });
    } finally {
      db.close();
    }
  }
  return {
    async read() {
      const value = await run("readonly", (store) => store.get(BEARER_KEY));
      return typeof value === "string" ? value : null;
    },
    async write(value) {
      await run("readwrite", (store) => store.put(value, BEARER_KEY));
    },
    async erase() {
      await run("readwrite", (store) => store.delete(BEARER_KEY));
    },
  };
}

function open(idb: IDBFactory): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = idb.open(BEARER_DB, 1);
    request.onupgradeneeded = () =>
      request.result.createObjectStore(BEARER_STORE);
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

/** The page's bearer. */
export const bearer = bearerStore(indexedDbBackend());
