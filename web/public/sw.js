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
// The shell routes below mirror `SPA_ROUTES` in `src/daemon/tcp.rs`: the
// server answers the index at exactly these paths, and so does this worker's
// offline fallback. A route in one and not the other works online and 404s
// from the home screen.

const CACHE = "efference-shell-v2";

/** Client routes the index answers, relative to the client's base — mirrors
 * `SPA_ROUTES` (src/daemon/tcp.rs). */
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
          keys.filter((key) => key !== CACHE).map((key) => caches.delete(key)),
        ),
      )
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
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
// The daemon sends when a gate pends or a turn settles. The payload is
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
    self.clients
      .matchAll({ type: "window", includeUncontrolled: true })
      .then((clients) => {
        // The sender already suppresses push while a stream connection is open
        // (docs/web-push.md), but it cannot see the races: the few seconds a
        // hidden tab lingers before dropping its stream, and a tab that has just
        // become visible and not yet reconnected. A visible window is a person
        // looking at the page, so the notification would be noise. Chrome exempts
        // this case from the penalty for a push that shows nothing.
        //
        // "Showing the session" is judged by path prefix rather than equality,
        // since the payload names the session's root (`/`, or `/sessions/<id>`
        // under a host) while the window may be on any route inside it. That is
        // also what keeps one session's notification from being swallowed by a
        // visible window on a different session of the same host.
        const root = new URL(payload.path, self.location.origin).pathname;
        const inSession = (pathname) =>
          pathname === root ||
          pathname.startsWith(root.endsWith("/") ? root : root + "/");
        const watching = clients.some(
          (client) =>
            client.visibilityState === "visible" &&
            inSession(new URL(client.url).pathname),
        );
        if (watching) return;
        return self.registration.showNotification(payload.title, {
          body: payload.body,
          data: { path: payload.path },
        });
      }),
  );
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const path = event.notification.data?.path ?? "/";
  const target = new URL(path, self.location.origin);
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
