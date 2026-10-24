// The service worker (docs/web-client.md "Installability"). Three jobs: make
// the app installable, let the shell paint from cache on a cold start or a lost
// network, and receive push (docs/web-push.md) — the handlers at the bottom of
// this file, which touch none of the caching rules above them.
//
// **Nothing session-shaped is ever cached.** `/v1`, `/files` and `/auth` go
// straight to the network, always — a transcript, a file's bytes, or a
// credential answer served from cache is a stale claim about what the agent
// did, and this page exists to say what the agent did. The cache holds the
// build's own static assets and the app shell, which say nothing about any
// session.
//
// The shell routes below mirror `SPA_ROUTES` in `src/daemon/tcp/gate.rs`: the
// server answers the index at exactly these paths, and so does this worker's
// offline fallback. A route in one and not the other works online and 404s
// from the home screen.

/** The precache's name. Bump it when a precached file's bytes change under the
 * same name (the manifest, the icons): an installed worker keeps the copies it
 * cached until a new name replaces them, and activation deletes every other
 * cache. */
const CACHE = "efference-shell-v3";

/** Dev mode: `src/pwa.ts` registers `/sw.js?dev=1` when the page is served by
 * Vite. The worker then caches nothing and answers no fetch, so HMR always
 * reaches the dev server; only the push handlers below are live. */
const DEV = new URL(self.location.href).searchParams.has("dev");

/** Prefix shared by every cache this worker has ever named, so dev mode can
 * clear a production worker's leftovers whatever version wrote them. */
const CACHE_PREFIX = "efference-shell-";

/** Client routes the index answers, relative to the client's base — mirrors
 * `SPA_ROUTES` (src/daemon/tcp/gate.rs). */
const SHELL_ROUTES = ["/", "/tree"];

/** A session prefix, which is the client's base when a host served the page
 * (api/base.ts, docs/host.md): every route above is also a route under it, and
 * every control path under it must reach the network. The id character set
 * mirrors `plain_id` (src/host.rs). */
const SESSION_PREFIX = /^\/sessions\/[A-Za-z0-9._-]+(?=\/|$)/;

/** A pathname with any session prefix removed, so one rule covers both
 * bindings: `/sessions/work/v1/state` -> `/v1/state`, `/sessions/work` -> `/`. */
const unprefixed = (pathname) => pathname.replace(SESSION_PREFIX, "") || "/";

const isShellRoute = (pathname) => SHELL_ROUTES.includes(unprefixed(pathname));

/** Public assets whose names the build does not hash, so they can be named
 * here. Everything else cacheable is a hashed `/assets/…` file, which is
 * cached when it is first fetched and never invalidated — the hash is the
 * version. */
const SHELL_ASSETS = [
  "/manifest.webmanifest",
  "/icon.svg",
  "/icon-192.png",
  "/icon-512.png",
  "/icon-maskable-512.png",
  "/apple-touch-icon.png",
  "/paper.svg",
];

/** Prefixes that must always reach the daemon. */
const LIVE = ["/v1", "/files", "/auth"];

const isLive = (pathname) => {
  const path = unprefixed(pathname);
  return LIVE.some(
    (prefix) => path === prefix || path.startsWith(prefix + "/"),
  );
};

const isCacheable = (pathname) =>
  pathname.startsWith("/assets/") ||
  pathname.startsWith("/fonts/") ||
  SHELL_ASSETS.includes(pathname);

self.addEventListener("install", (event) => {
  if (DEV) {
    self.skipWaiting();
    return;
  }
  event.waitUntil(
    caches
      .open(CACHE)
      // Best-effort: a failed precache must not leave the app uninstallable,
      // and every entry here is re-fetchable on demand anyway.
      .then((cache) => cache.addAll([...SHELL_ROUTES, ...SHELL_ASSETS]))
      .catch(() => undefined),
  );
  self.skipWaiting();
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) =>
        Promise.all(
          keys
            .filter((key) =>
              DEV ? key.startsWith(CACHE_PREFIX) : key !== CACHE,
            )
            .map((key) => caches.delete(key)),
        ),
      )
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
  if (DEV) return;
  const request = event.request;
  if (request.method !== "GET") return;
  const url = new URL(request.url);
  if (url.origin !== self.location.origin) return;
  if (isLive(url.pathname)) return;

  // A navigation to a shell route is answered from the network when there is
  // one, and from the cached shell when there is not — so a cold launch offline
  // paints the app, which then says it cannot reach the session rather than
  // showing nothing. A session-prefixed route falls back to the same shell:
  // only its own path is precached, and the shell is the same bytes either way.
  if (request.mode === "navigate" && isShellRoute(url.pathname)) {
    event.respondWith(
      fetch(request)
        .then((response) => {
          if (response.ok) void store(request, response.clone());
          return response;
        })
        .catch(
          async () =>
            (await caches.match(request)) ?? (await caches.match("/")),
        ),
    );
    return;
  }

  if (!isCacheable(url.pathname)) return;
  // Hashed assets are immutable, so cache-first is correct rather than merely
  // fast: a name that is in the cache is the exact bytes that name means.
  event.respondWith(
    caches.match(request).then(
      (hit) =>
        hit ??
        fetch(request).then((response) => {
          if (response.ok) void store(request, response.clone());
          return response;
        }),
    ),
  );
});

async function store(request, response) {
  const cache = await caches.open(CACHE);
  await cache.put(request, response);
}

// ---- push (docs/web-push.md) ----
//
// The daemon sends when a gate pends, an `ask` is posed, or a turn settles, and
// only once nobody attended to it (docs/web-push.md), so every push is shown. The payload is
// `{title, body, path}`; `path` is where a click goes, named by the sender
// rather than assumed here, so a host serving sessions under a prefix only
// changes the sender.

self.addEventListener("push", (event) => {
  // A push with no readable payload still shows something: the platforms
  // require a visible notification for every delivery (`userVisibleOnly`), and
  // a silent drop spends the browser's budget for showing one later.
  let payload = {
    title: "efference",
    body: "the session wants you",
    path: "/",
  };
  try {
    payload = { ...payload, ...event.data.json() };
  } catch {
    // keep the default
  }
  event.waitUntil(
    self.registration.showNotification(payload.title, {
      body: payload.body,
      data: { path: payload.path },
    }),
  );
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const path = event.notification.data?.path ?? "/";
  let target = new URL(path, self.location.origin);
  // The path is the push sender's choice, not ours: an absolute URL —
  // `https://…`, `//…`, `javascript:…` — overrides the base origin in that
  // constructor. Anything that resolves off-origin is replaced with the app
  // root; `openWindow` and the focus match below then stay on this origin.
  if (target.origin !== self.location.origin) {
    target = new URL("/", self.location.origin);
  }
  event.waitUntil(
    self.clients
      .matchAll({ type: "window", includeUncontrolled: true })
      .then((clients) => {
        // A window already on that path is focused rather than duplicated —
        // the notification is a summons back to a page that is probably still
        // open, not a request for a second copy of it.
        const open = clients.find(
          (client) => new URL(client.url).pathname === target.pathname,
        );
        return open ? open.focus() : self.clients.openWindow(target.href);
      }),
  );
});

// A push service can expire or rotate a subscription; the browser then fires
// this with the old one (when it still has it) and expects a new one. The new
// subscription goes to the daemon here, and the page's resubscribe-on-open
// (web/src/push.ts) covers whatever this misses.
self.addEventListener("pushsubscriptionchange", (event) => {
  event.waitUntil(resubscribe(event.oldSubscription ?? null));
});

/** The control base to post to. The worker has no page of its own, so it reads
 * the base off an open window (`/sessions/<id>` under a host, mirroring
 * `deriveBase` in src/api/base.ts); with none open it uses the root, which a
 * session daemon answers and a host does not — that case is repaired by the
 * next app open. */
async function controlBase() {
  const windows = await self.clients.matchAll({
    type: "window",
    includeUncontrolled: true,
  });
  for (const client of windows) {
    const match = /^\/sessions\/[A-Za-z0-9._-]+(?=\/|$)/.exec(
      new URL(client.url).pathname,
    );
    if (match) return match[0];
  }
  return "";
}

/** Where the page keeps its bearer: the database, store, and key that
 * `BEARER_DB`, `BEARER_STORE`, and `BEARER_KEY` name in src/api/bearer.ts;
 * src/pwa.test.ts pins that the two spellings agree. */
const BEARER_DB = "efference-auth";
const BEARER_STORE = "credentials";
const BEARER_KEY = "bearer";

/** The page's bearer, or `null` when none is stored or storage cannot be read.
 * Opened without a version, so whatever schema the page wrote is the one read;
 * a database the page has not created yet is created with the page's store, so
 * the page's own open finds what it expects. */
function storedBearer() {
  return new Promise((resolve) => {
    try {
      const request = self.indexedDB.open(BEARER_DB);
      request.onupgradeneeded = () =>
        request.result.createObjectStore(BEARER_STORE);
      request.onerror = () => resolve(null);
      request.onsuccess = () => {
        const db = request.result;
        try {
          const read = db
            .transaction(BEARER_STORE)
            .objectStore(BEARER_STORE)
            .get(BEARER_KEY);
          read.onsuccess = () =>
            resolve(typeof read.result === "string" ? read.result : null);
          read.onerror = () => resolve(null);
        } catch {
          resolve(null);
        } finally {
          // Closes once the read's transaction completes.
          db.close();
        }
      };
    } catch {
      resolve(null);
    }
  });
}

async function resubscribe(old) {
  try {
    const base = await controlBase();
    // The routes below are under the `/v1` gate (docs/web-push.md). With no
    // bearer stored they answer `401`, and the page's resubscribe-on-open
    // repairs the row instead.
    const token = await storedBearer();
    const credential =
      token === null ? {} : { authorization: `Bearer ${token}` };
    let key = old?.options?.applicationServerKey ?? null;
    if (!key) {
      const response = await fetch(`${base}/v1/push`, { headers: credential });
      if (!response.ok) return;
      const { key: b64 } = await response.json();
      const padded = b64
        .replace(/-/g, "+")
        .replace(/_/g, "/")
        .padEnd(Math.ceil(b64.length / 4) * 4, "=");
      key = Uint8Array.from(self.atob(padded), (c) => c.charCodeAt(0));
    }
    const subscription = await self.registration.pushManager.subscribe({
      userVisibleOnly: true,
      applicationServerKey: key,
    });
    await fetch(`${base}/v1/push/subscriptions`, {
      method: "POST",
      headers: { ...credential, "content-type": "application/json" },
      body: JSON.stringify(subscription.toJSON()),
    });
    if (old && old.endpoint !== subscription.endpoint) {
      await fetch(`${base}/v1/push/subscriptions`, {
        method: "DELETE",
        headers: { ...credential, "content-type": "application/json" },
        body: JSON.stringify({ endpoint: old.endpoint }),
      });
    }
  } catch (error) {
    self.console.warn("pushsubscriptionchange:", error);
  }
}
