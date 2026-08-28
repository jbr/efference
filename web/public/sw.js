// The service worker (docs/web-client.md "Installability"). Two jobs, and no
// third: make the app installable, and let the shell paint from cache on a
// cold start or a lost network.
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

/** Client routes the index answers — mirrors `SPA_ROUTES` (src/daemon/tcp.rs). */
const SHELL_ROUTES = ["/", "/tree"];

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

const isLive = (pathname) =>
  LIVE.some(
    (prefix) => pathname === prefix || pathname.startsWith(prefix + "/"),
  );

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

  // A navigation is answered from the network when there is one, and from the
  // cached shell when there is not — so a cold launch offline paints the app,
  // which then says it cannot reach the session rather than showing nothing.
  if (request.mode === "navigate") {
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
