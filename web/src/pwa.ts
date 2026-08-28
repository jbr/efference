// Installability (docs/web-client.md): registering the service worker in
// `public/sw.js`, which is what makes the app installable and lets the shell
// paint offline.
//
// Production only. Under Vite the same file would sit in front of the dev
// server's module graph, and a cached asset is exactly what HMR must not get;
// an unregister on the dev path also clears a worker left behind by a
// production build served from the same origin, which is otherwise a
// hard-to-see cause of "my change did nothing".

export function registerServiceWorker(): void {
  if (!("serviceWorker" in navigator)) return;
  if (!import.meta.env.PROD) {
    void navigator.serviceWorker
      .getRegistrations()
      .then((registrations) => {
        for (const registration of registrations)
          void registration.unregister();
      })
      .catch(() => undefined);
    return;
  }
  // After load: registration competes with the session's own first fetches
  // otherwise, and nothing on screen waits for it.
  window.addEventListener("load", () => {
    void navigator.serviceWorker
      .register("/sw.js", { scope: "/" })
      // A failed registration costs installability and nothing else — the app
      // is a normal page either way — so it is logged, not surfaced.
      .catch((error: unknown) => console.warn("service worker:", error));
  });
}
