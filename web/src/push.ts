// The page's side of Web Push beyond the opt-in click (docs/web-push.md): finding
// the worker's registration, reading this device's subscription state, and
// re-sending a held subscription to the daemon on every app open.

import { subscribePush } from "./api/http.ts";

/** How long to wait for a service worker before concluding there is none.
 * `navigator.serviceWorker.ready` never settles when no worker is registered
 * (a failed registration, an insecure origin), so absence is only observable
 * as a timeout. */
export const WORKER_WAIT_MS = 5000;

/** The minimal container surface used here, so tests can stand in for it. */
export interface WorkerContainer {
  ready: Promise<{
    pushManager: {
      getSubscription(): Promise<{ toJSON(): PushSubscriptionJSON } | null>;
    };
  }>;
}

type Registration = Awaited<WorkerContainer["ready"]>;

/** The active registration, or `null` when none becomes ready within `ms`. */
export function workerReady(
  container: WorkerContainer | undefined,
  ms: number = WORKER_WAIT_MS,
): Promise<Registration | null> {
  if (!container) return Promise.resolve(null);
  return new Promise((resolve) => {
    const timer = setTimeout(() => resolve(null), ms);
    container.ready.then(
      (registration) => {
        clearTimeout(timer);
        resolve(registration);
      },
      () => {
        clearTimeout(timer);
        resolve(null);
      },
    );
  });
}

/** What the notifications checkbox knows about this device: `pending` until the
 * worker answers, `unavailable` when there is no worker to ask. */
export type PushState =
  "pending" | "subscribed" | "unsubscribed" | "unavailable";

/** Read this device's subscription state from the browser, the source of truth. */
export async function readPushState(
  container: WorkerContainer | undefined,
  ms: number = WORKER_WAIT_MS,
): Promise<PushState> {
  const registration = await workerReady(container, ms);
  if (!registration) return "unavailable";
  try {
    const subscription = await registration.pushManager.getSubscription();
    return subscription ? "subscribed" : "unsubscribed";
  } catch {
    return "unsubscribed";
  }
}

/** Re-send a held subscription to the daemon. The daemon's store is an upsert
 * on the endpoint, so this is harmless when the row exists and refills it after
 * any loss (the database move, a pruned row, a rotated root). Failures are
 * logged, not surfaced: the page works without push. */
export async function resubscribeOnOpen(
  container: WorkerContainer | undefined,
  send: (
    subscription: PushSubscriptionJSON,
  ) => Promise<unknown> = subscribePush,
  ms: number = WORKER_WAIT_MS,
): Promise<void> {
  try {
    const registration = await workerReady(container, ms);
    if (!registration) return;
    const subscription = await registration.pushManager.getSubscription();
    if (subscription) await send(subscription.toJSON());
  } catch (error) {
    console.warn("push resubscribe:", error);
  }
}
