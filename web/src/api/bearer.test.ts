// The page's bearer store (api/bearer.ts) over stand-in backends: what it holds
// in memory, what reaches storage and in which order, and that storage which
// is missing, throws, or never answers costs persistence and nothing else; and
// that subscribers hear every change of what is held.

import { expect, test, vi } from "vitest";
import { bearerStore, indexedDbBackend, type BearerBackend } from "./bearer.ts";

/** A backend over one in-memory slot — what IndexedDB is to the page. */
function slot(initial: string | null = null) {
  const state = { value: initial };
  const backend: BearerBackend = {
    read: async () => state.value,
    write: async (value) => {
      state.value = value;
    },
    erase: async () => {
      state.value = null;
    },
  };
  return { state, backend };
}

/** A backend for which every call fails, as storage does in some private
 * modes. */
const broken: BearerBackend = {
  read: () => Promise.reject(new Error("unavailable")),
  write: () => Promise.reject(new Error("unavailable")),
  erase: () => Promise.reject(new Error("unavailable")),
};

test("a load holds what storage holds, and a store persists for the next load", async () => {
  const { state, backend } = slot("from-before");
  const page = bearerStore(backend);
  expect(page.held()).toBeNull();
  await page.load();
  expect(page.held()).toBe("from-before");

  await page.store("fresh");
  expect(page.held()).toBe("fresh");
  expect(state.value).toBe("fresh");

  const reloaded = bearerStore(backend);
  await reloaded.load();
  expect(reloaded.held()).toBe("fresh");
});

test("a clear forgets the held bearer and erases the stored one", async () => {
  const { state, backend } = slot("signed-in");
  const page = bearerStore(backend);
  await page.load();
  await page.clear();
  expect(page.held()).toBeNull();
  expect(state.value).toBeNull();
});

test("storage that fails leaves memory working and rejects nothing", async () => {
  const page = bearerStore(broken);
  await expect(page.load()).resolves.toBeUndefined();
  expect(page.held()).toBeNull();
  await expect(page.store("memory-only")).resolves.toBeUndefined();
  expect(page.held()).toBe("memory-only");
  await expect(page.clear()).resolves.toBeUndefined();
  expect(page.held()).toBeNull();
});

test("a read that never answers lets the load finish with nothing held", async () => {
  const page = bearerStore({
    ...slot().backend,
    read: () => new Promise<never>(() => {}),
  });
  await page.load(10);
  expect(page.held()).toBeNull();
});

test("writes reach storage in call order, so a slow clear cannot erase a later sign-in", async () => {
  const { state, backend } = slot("stale");
  let finishErase = () => {};
  const page = bearerStore({
    ...backend,
    erase: () =>
      new Promise<void>((resolve) => {
        finishErase = () => {
          state.value = null;
          resolve();
        };
      }),
  });
  const cleared = page.clear();
  const stored = page.store("fresh");
  // The erase is still out, and the write waits behind it.
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(state.value).toBe("stale");
  finishErase();
  await Promise.all([cleared, stored]);
  expect(state.value).toBe("fresh");
  expect(page.held()).toBe("fresh");
});

test("a load that answers after a sign-in leaves the newer bearer held", async () => {
  let answer: (value: string | null) => void = () => {};
  const page = bearerStore({
    ...slot().backend,
    read: () =>
      new Promise((resolve) => {
        answer = resolve;
      }),
  });
  const loaded = page.load();
  await page.store("fresh");
  answer("older");
  await loaded;
  expect(page.held()).toBe("fresh");
});

test("a subscriber hears each change once the held bearer has moved, until it stops", async () => {
  const page = bearerStore(slot().backend);
  const heard: (string | null)[] = [];
  const stop = page.subscribe(() => heard.push(page.held()));

  await page.store("first");
  await page.store("second");
  await page.clear();
  expect(heard).toEqual(["first", "second", null]);

  stop();
  await page.store("unheard");
  expect(heard).toEqual(["first", "second", null]);
});

test("a listener that stores in turn leaves storage agreeing with memory", async () => {
  const { state, backend } = slot();
  const page = bearerStore(backend);
  let inner: Promise<void> = Promise.resolve();
  page.subscribe(() => {
    if (page.held() === "first") inner = page.store("second");
  });

  await page.store("first");
  await inner;

  expect(page.held()).toBe("second");
  expect(state.value).toBe("second");
});

test("nothing is announced when the held bearer stays what it was", async () => {
  const page = bearerStore(slot().backend);
  const listener = vi.fn();
  page.subscribe(listener);
  await page.clear();
  await page.store("same");
  await page.store("same");
  expect(listener).toHaveBeenCalledTimes(1);
});

test("one function subscribed twice is two subscriptions, each stopped on its own", async () => {
  const page = bearerStore(slot().backend);
  const listener = vi.fn();
  const stopFirst = page.subscribe(listener);
  page.subscribe(listener);
  await page.store("a");
  expect(listener).toHaveBeenCalledTimes(2);
  stopFirst();
  await page.store("b");
  expect(listener).toHaveBeenCalledTimes(3);
});

test("a load announces the bearer it finds, and one a sign-in overtook announces nothing", async () => {
  const listener = vi.fn();
  const loaded = bearerStore(slot("from-before").backend);
  loaded.subscribe(listener);
  await loaded.load();
  expect(listener).toHaveBeenCalledTimes(1);

  let answer: (value: string | null) => void = () => {};
  const overtaken = bearerStore({
    ...slot().backend,
    read: () =>
      new Promise((resolve) => {
        answer = resolve;
      }),
  });
  const heard = vi.fn();
  const pending = overtaken.load();
  overtaken.subscribe(heard);
  await overtaken.store("fresh");
  answer("older");
  await pending;
  expect(heard).toHaveBeenCalledTimes(1);
  expect(overtaken.held()).toBe("fresh");
});

test("an IndexedDB that is missing or throws on access fails the call, and the store falls back to memory", async () => {
  for (const factory of [
    () => undefined,
    () => {
      throw new DOMException("denied", "SecurityError");
    },
  ]) {
    const backend = indexedDbBackend(factory);
    await expect(backend.read()).rejects.toThrow();
    await expect(backend.write("t")).rejects.toThrow();
    await expect(backend.erase()).rejects.toThrow();

    const page = bearerStore(backend);
    await page.load();
    await page.store("memory-only");
    expect(page.held()).toBe("memory-only");
  }
});
