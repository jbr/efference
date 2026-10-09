import { describe, expect, it, vi } from "vitest";
import {
  readPushState,
  resubscribeOnOpen,
  type WorkerContainer,
} from "./push.ts";

const json = {
  endpoint: "https://push.example/a",
  keys: { p256dh: "p", auth: "a" },
};

const container = (sub: unknown): WorkerContainer => ({
  ready: Promise.resolve({
    pushManager: {
      getSubscription: () =>
        Promise.resolve(sub as { toJSON(): PushSubscriptionJSON } | null),
    },
  }),
});

const never: WorkerContainer = { ready: new Promise(() => undefined) };

describe("resubscribe on open", () => {
  it("sends a held subscription", async () => {
    const send = vi.fn().mockResolvedValue(undefined);
    await resubscribeOnOpen(container({ toJSON: () => json }), send, 50);
    expect(send).toHaveBeenCalledWith(json);
  });

  it("sends nothing without a subscription or a worker", async () => {
    const send = vi.fn();
    await resubscribeOnOpen(container(null), send, 50);
    await resubscribeOnOpen(never, send, 10);
    await resubscribeOnOpen(undefined, send, 10);
    expect(send).not.toHaveBeenCalled();
  });

  it("logs a failed send instead of throwing", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const send = vi.fn().mockRejectedValue(new Error("401"));
    await expect(
      resubscribeOnOpen(container({ toJSON: () => json }), send, 50),
    ).resolves.toBeUndefined();
    expect(warn).toHaveBeenCalled();
    warn.mockRestore();
  });
});

describe("push state", () => {
  it("reads the three outcomes", async () => {
    expect(await readPushState(container({ toJSON: () => json }), 50)).toBe(
      "subscribed",
    );
    expect(await readPushState(container(null), 50)).toBe("unsubscribed");
    expect(await readPushState(never, 10)).toBe("unavailable");
    expect(await readPushState(undefined, 10)).toBe("unavailable");
  });
});
