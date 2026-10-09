// Installability (docs/web-client.md): registering the service worker in
// `public/sw.js`, which is what makes the app installable and lets the shell
// paint offline.
//
// Production registers the caching worker. Under Vite it registers the same
// file as `/sw.js?dev=1`, which caches nothing and answers no fetch, so HMR
// never gets a cached asset while push still has a worker to live in; the
// differing script URL also replaces a production worker left behind on the
// same origin, and the dev worker clears that worker's caches on activation.

/** The worker script URL for a build: the query is the mode switch `sw.js`
 * reads. */
export const workerUrl = (prod: boolean): string =>
  prod ? "/sw.js" : "/sw.js?dev=1";

export function registerServiceWorker(): void {
  if (!("serviceWorker" in navigator)) return;
  // After load: registration competes with the session's own first fetches
  // otherwise, and nothing on screen waits for it.
  window.addEventListener("load", () => {
    void navigator.serviceWorker
      .register(workerUrl(import.meta.env.PROD), { scope: "/" })
      // A failed registration costs installability and nothing else — the app
      // is a normal page either way — so it is logged, not surfaced.
      .catch((error: unknown) => console.warn("service worker:", error));
  });
}
