// @vitest-environment jsdom
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type { Attachment } from "../api/types.ts";
import { DraftStore } from "./drafts.ts";

const attachment: Attachment = {
  name: "note.txt",
  path: "/stored/note.txt",
  kind: { Binary: { bytes: 5 } },
};
const image: Attachment = {
  name: "image.png",
  path: "/stored/image.png",
  kind: { Image: { format: "PNG", width: 1, height: 1, bytes: 5 } },
};
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
function setup(base = "/sessions/a") {
  const pending = deferred<Attachment>();
  const upload = vi.fn(() => pending.promise);
  const remove = vi.fn(() => Promise.resolve());
  const store = new DraftStore(base, { storage: localStorage, upload, remove });
  return { store, upload, remove, pending };
}

beforeEach(() => {
  const values = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => void values.set(key, value),
    removeItem: (key: string) => void values.delete(key),
    key: (index: number) => [...values.keys()][index] ?? null,
    get length() {
      return values.size;
    },
  });
  history.replaceState(null, "", "/sessions/a/");
  vi.stubGlobal(
    "URL",
    class extends URL {
      static createObjectURL = vi.fn(() => "blob:preview");
      static revokeObjectURL = vi.fn();
    },
  );
});
afterEach(() => vi.unstubAllGlobals());

test("text and completed attachments restore independently for each session", () => {
  const a = setup().store;
  const b = setup("/sessions/b").store;
  a.setText("  a draft\n");
  a.restore([image]);
  b.setText("b draft");
  const restored = setup().store.snapshot();
  expect(restored.text).toBe("  a draft\n");
  expect(restored.staged[0]).toMatchObject({
    status: "ready",
    attachment: image,
    preview: "/sessions/a/v1/attachments/image.png",
  });
  expect(setup("/sessions/b").store.snapshot().text).toBe("b draft");
});

test("uploads finish in their original draft with no mounted subscriber", async () => {
  const { store, pending } = setup();
  const listener = vi.fn();
  const unsubscribe = store.subscribe(listener);
  store.stage(new File(["bytes"], "note.txt"));
  unsubscribe();
  listener.mockClear();
  history.replaceState(null, "", "/sessions/b/");
  const b = setup("/sessions/b").store;
  pending.resolve(attachment);
  await pending.promise;
  expect(listener).not.toHaveBeenCalled();
  expect(store.snapshot().staged[0]).toMatchObject({
    status: "ready",
    attachment,
  });
  expect(b.snapshot().staged).toEqual([]);
  expect(setup().store.snapshot().staged[0]?.attachment).toEqual(attachment);
});

test("reload keeps upload presence and pasted text but never file bytes or blob URLs", () => {
  const { store } = setup();
  store.stage(
    new File(["secret file bytes"], "image.png", { type: "image/png" }),
    "pasted text",
  );
  const raw = localStorage.getItem(localStorage.key(0)!);
  expect(raw).not.toContain("secret file bytes");
  expect(raw).not.toContain("blob:");
  const restored = setup().store.snapshot().staged[0];
  expect(restored).toMatchObject({
    status: "failed",
    label: store.snapshot().staged[0].label,
    pastedText: "pasted text",
    preview: null,
  });
  expect(restored.error).toContain("interrupted");
  expect(restored.attachment).toBeUndefined();
});

test("removing a pending upload deletes its eventual stored result without reviving the chip", async () => {
  const { store, pending, remove } = setup();
  store.stage(new File(["bytes"], "note.txt"));
  store.remove(store.snapshot().staged[0].key);
  pending.resolve(attachment);
  await pending.promise;
  expect(remove).toHaveBeenCalledExactlyOnceWith("note.txt");
  expect(store.snapshot().staged).toEqual([]);
  expect(localStorage.length).toBe(0);
});

test("failed uploads remain removable and survive reload", async () => {
  const { store, pending } = setup();
  store.stage(new File(["bytes"], "note.txt"));
  pending.reject(new Error("offline"));
  await pending.promise.catch(() => {});
  expect(store.snapshot().staged[0]).toMatchObject({
    status: "failed",
    error: "Error: offline",
  });
  const restored = setup().store;
  expect(restored.snapshot().staged[0]?.error).toBe("Error: offline");
  restored.remove(restored.snapshot().staged[0].key);
  expect(localStorage.length).toBe(0);
});

test("successful send clears the sent draft but does not delete its uploaded files", () => {
  const { store, remove } = setup();
  store.setText("send this");
  store.restore([attachment]);
  store.sent(store.snapshot());
  expect(store.snapshot()).toEqual({ text: "", staged: [] });
  expect(localStorage.length).toBe(0);
  expect(remove).not.toHaveBeenCalled();
});

test("a late successful send keeps newer edits, attachments, and failed chips", () => {
  const { store } = setup();
  store.setText("first");
  store.restore([attachment]);
  const submitted = store.snapshot();
  store.setText("next draft");
  store.restore([image]);
  store.sent(submitted);
  expect(store.snapshot().text).toBe("next draft");
  expect(store.snapshot().staged.map((s) => s.attachment)).toEqual([image]);
});

test("a failed chip is not silently discarded when sending text", () => {
  const { store } = setup();
  store.stage(new File(["bytes"], "note.txt"));
  const restored = setup().store;
  restored.setText("text only");
  restored.sent(restored.snapshot());
  expect(restored.snapshot().text).toBe("");
  expect(restored.snapshot().staged[0]?.status).toBe("failed");
});

test("image previews are revoked on upload completion and use the original session URL", async () => {
  const { store, pending } = setup();
  store.stage(new File(["bytes"], "image.png", { type: "image/png" }));
  history.replaceState(null, "", "/sessions/b/");
  pending.resolve(image);
  await pending.promise;
  expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:preview");
  expect(store.snapshot().staged[0]?.preview).toBe(
    "/sessions/a/v1/attachments/image.png",
  );
});

test("default upload and a late removal remain bound to the original API base", async () => {
  const response = deferred<Response>();
  const fetchMock = vi
    .fn()
    .mockImplementationOnce(() => response.promise)
    .mockResolvedValue(new Response(null, { status: 204 }));
  vi.stubGlobal("fetch", fetchMock);
  const store = new DraftStore("/sessions/a");
  store.stage(new File(["bytes"], "note.txt"));
  expect(fetchMock.mock.calls[0][0]).toBe(
    "/sessions/a/v1/attachments?name=note.txt",
  );
  store.remove(store.snapshot().staged[0].key);
  history.replaceState(null, "", "/sessions/b/");
  response.resolve(new Response(JSON.stringify(attachment), { status: 200 }));
  await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
  expect(fetchMock.mock.calls[1][0]).toBe(
    "/sessions/a/v1/attachments/note.txt",
  );
});

test("malformed stored data is ignored without breaking the composer", () => {
  localStorage.setItem("efference.composer.v1:/sessions/a", "{");
  expect(setup().store.snapshot()).toEqual({ text: "", staged: [] });
  localStorage.setItem(
    "efference.composer.v1:/sessions/a",
    JSON.stringify({
      text: "kept",
      staged: [
        null,
        2,
        {},
        {
          label: "bad.png",
          status: "ready",
          attachment: { name: "bad", path: "bad", kind: { Image: null } },
        },
      ],
    }),
  );
  expect(setup().store.snapshot().text).toBe("kept");
  expect(setup().store.snapshot().staged[0]).toMatchObject({
    status: "failed",
    preview: null,
  });
});

test("denied or full storage does not break editing and staging", () => {
  const storage = {
    getItem: () => {
      throw new Error("denied");
    },
    setItem: () => {
      throw new Error("full");
    },
    removeItem: () => {
      throw new Error("denied");
    },
  };
  const store = new DraftStore("/sessions/a", {
    storage,
    upload: () => Promise.resolve(attachment),
    remove: () => Promise.resolve(),
  });
  store.setText("still works");
  store.restore([attachment]);
  expect(store.snapshot().text).toBe("still works");
  expect(store.snapshot().staged).toHaveLength(1);
  store.sent(store.snapshot());
  expect(store.snapshot()).toEqual({ text: "", staged: [] });
});
