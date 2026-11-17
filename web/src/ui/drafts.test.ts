// @vitest-environment jsdom
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { ApiError } from "../api/http.ts";
import type { ReferenceDefinition, UserContent } from "../api/types.ts";
import { DraftStore } from "./drafts.ts";
import {
  attachment,
  attachmentDocument,
  deferred,
  image,
  installBrowser,
  occurrence,
  setup,
  unmount,
} from "./drafts/fixtures.ts";

beforeEach(installBrowser);
afterEach(() => vi.unstubAllGlobals());

test("text and draft content stay independent for each session", () => {
  const a = setup().store;
  const b = setup("/sessions/b").store;
  a.setText("  a draft\n");
  const key = a.uploadFile(new File(["bytes"], "note.txt"));
  a.setContent({ text: "", document: occurrence(key) });
  b.setText("b draft");
  const restored = setup().store.snapshot();
  expect(restored.text).toBe("");
  expect(restored.document).toEqual(occurrence(restored.uploads[0]!.key));
  expect(setup("/sessions/b").store.snapshot().text).toBe("b draft");
});

test("an upload lands in its original draft with no mounted subscriber", async () => {
  const { store, pending } = setup();
  const listener = vi.fn();
  const unsubscribe = store.subscribe(listener);
  const key = store.uploadFile(new File(["bytes"], "note.txt"));
  store.setContent({ text: "", document: occurrence(key) });
  unsubscribe();
  listener.mockClear();
  history.replaceState(null, "", "/sessions/b/");
  const b = setup("/sessions/b").store;
  pending.resolve(attachment);
  await pending.promise;
  expect(listener).not.toHaveBeenCalled();
  expect(store.snapshot().uploads[0]).toMatchObject({
    status: "ready",
    attachment,
  });
  expect(b.snapshot().uploads).toEqual([]);
});

test("reload keeps upload presence and pasted text but never file bytes or blob URLs", () => {
  const { store } = setup();
  store.beginEditorSession();
  const key = store.uploadFile(
    new File(["secret file bytes"], "image.png", { type: "image/png" }),
    "pasted text",
  );
  store.setContent({ text: "", document: occurrence(key) });
  const raw = localStorage.getItem(localStorage.key(0)!);
  expect(raw).not.toContain("secret file bytes");
  expect(raw).not.toContain("blob:");
  const restored = setup().store.snapshot().uploads[0];
  expect(restored).toMatchObject({
    status: "failed",
    label: store.snapshot().uploads[0].label,
    pastedText: "pasted text",
    preview: null,
  });
  expect(restored.error).toContain("interrupted");
  expect(restored.attachment).toBeUndefined();
});

test("removing a pending upload detaches it now and deletes the landing bytes only after teardown", async () => {
  const { store, pending, remove } = setup();
  store.beginEditorSession();
  const key = store.uploadFile(new File(["bytes"], "note.txt"));
  store.setContent({ text: "", document: occurrence(key) });
  store.remove(key);
  expect(store.snapshot().document).toBeNull();
  // Detached, not deleted: undo can still restore the chip.
  expect(store.snapshot().uploads[0]).toMatchObject({
    key,
    status: "uploading",
  });
  pending.resolve(attachment);
  await pending.promise;
  expect(remove).not.toHaveBeenCalled();
  expect(store.snapshot().uploads[0]).toMatchObject({
    status: "ready",
    attachment,
  });
  await unmount(store);
  expect(remove).toHaveBeenCalledExactlyOnceWith("note.txt");
  expect(store.snapshot().uploads).toEqual([]);
});

test("a pending upload pruned while in flight deletes its landing bytes and revives no chip", async () => {
  const { store, pending, remove } = setup();
  store.beginEditorSession();
  const key = store.uploadFile(new File(["bytes"], "note.txt"));
  store.setContent({ text: "", document: occurrence(key) });
  store.remove(key);
  await unmount(store);
  expect(store.snapshot().uploads).toEqual([]);
  pending.resolve(attachment);
  await pending.promise;
  expect(remove).toHaveBeenCalledExactlyOnceWith("note.txt");
  expect(store.snapshot().uploads).toEqual([]);
});

test("an undo restoring the chip before the upload lands keeps it live and completing in place", async () => {
  const { store, pending, remove } = setup();
  store.beginEditorSession();
  const key = store.uploadFile(new File(["bytes"], "note.txt"));
  store.setContent({ text: "", document: occurrence(key) });
  store.remove(key);
  // The editor's undo dispatches the removed content back.
  store.setContent({ text: "", document: occurrence(key) });
  pending.resolve(attachment);
  await pending.promise;
  expect(store.snapshot().uploads[0]).toMatchObject({
    status: "ready",
    attachment,
  });
  await unmount(store);
  expect(remove).not.toHaveBeenCalled();
});

test("failed uploads stay visible across reload and are dropped without a delete", async () => {
  const { store, pending, remove } = setup();
  store.beginEditorSession();
  const key = store.uploadFile(new File(["bytes"], "note.txt"));
  store.setContent({ text: "", document: occurrence(key) });
  pending.reject(new Error("offline"));
  await pending.promise.catch(() => {});
  expect(store.snapshot().uploads[0]).toMatchObject({
    status: "failed",
    error: "Upload failed (offline); attach this file again.",
  });
  const restored = setup().store;
  expect(restored.snapshot().uploads[0]?.error).toBe(
    "Upload failed (offline); attach this file again.",
  );
  restored.beginEditorSession();
  restored.setContent({ text: "", document: null });
  await unmount(restored);
  expect(remove).not.toHaveBeenCalled();
});

test("a failed upload names the daemon's reason", async () => {
  const { store, pending } = setup();
  const key = store.uploadFile(new File(["bytes"], "note.txt"));
  store.setContent({ text: "", document: occurrence(key) });
  pending.reject(
    new ApiError(500, {
      code: "internal",
      message: "storing the upload: disk full",
    }),
  );
  await pending.promise.catch(() => {});
  expect(store.snapshot().uploads[0]?.error).toBe(
    "Upload failed (storing the upload: disk full); attach this file again.",
  );
});

test("a successful send clears the sent draft, marks its uploads sent, and deletes nothing", async () => {
  const { store, remove } = setup();
  store.beginEditorSession();
  const key = store.uploadFile(new File(["bytes"], "note.txt"));
  store.setContent({ text: "", document: occurrence(key) });
  const submitted = store.beginSend(store.snapshot());
  store.sent(submitted);
  expect(store.snapshot()).toMatchObject({ text: "", document: null });
  expect(store.snapshot().uploads.find((u) => u.key === key)).toMatchObject({
    provenance: "sent",
    pinned: false,
  });
  await unmount(store);
  expect(remove).not.toHaveBeenCalled();
});

test("a late successful send keeps newer edits and uploads", () => {
  const { store } = setup();
  store.setText("first");
  const submitted = store.snapshot();
  store.setText("next draft");
  store.sent(submitted);
  expect(store.snapshot().text).toBe("next draft");
});

test("a failed send retains the uploads it named and releases their pins", async () => {
  const { store, remove, pending } = setup();
  store.beginEditorSession();
  const key = store.uploadFile(new File(["bytes"], "note.txt"));
  store.setContent({ text: "", document: occurrence(key) });
  pending.resolve(attachment);
  await pending.promise;
  const submitted = store.beginSend(store.snapshot());
  store.sendFailed(submitted);
  expect(store.snapshot().uploads[0]).toMatchObject({
    provenance: "sent",
    pinned: false,
  });
  expect(store.snapshot().document).toEqual(occurrence(key));
  // Nothing is deleted while the draft still shows it…
  store.setContent({ text: "", document: null });
  await unmount(store);
  // …and even detached, a possibly-admitted upload's bytes stay.
  expect(remove).not.toHaveBeenCalled();
});

test("removing an occurrence during a pinned send detaches it and keeps its bytes", async () => {
  const { store, remove, pending } = setup();
  store.beginEditorSession();
  const key = store.uploadFile(new File(["bytes"], "note.txt"));
  store.setContent({ text: "", document: occurrence(key) });
  pending.resolve(attachment);
  await pending.promise;
  const submitted = store.beginSend(store.snapshot());
  // A pin protects the bytes from cleanup, not the draft from editing: a cut
  // or backspace detaches the same occurrence without consulting it.
  store.remove(key);
  expect(store.snapshot().document).toBeNull();
  expect(store.snapshot().uploads[0]).toMatchObject({
    status: "ready",
    pinned: true,
  });
  store.sendFailed(submitted);
  expect(store.snapshot().uploads[0]).toMatchObject({
    provenance: "sent",
    pinned: false,
  });
  await unmount(store);
  expect(remove).not.toHaveBeenCalled();
});

test("image previews are revoked on upload completion and use the original session URL", async () => {
  const { store, pending } = setup();
  const key = store.uploadFile(
    new File(["bytes"], "image.png", { type: "image/png" }),
  );
  store.setContent({ text: "", document: occurrence(key) });
  history.replaceState(null, "", "/sessions/b/");
  pending.resolve(image);
  await pending.promise;
  expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:preview");
  expect(store.snapshot().uploads[0]?.preview).toBe(
    "/sessions/a/v1/attachments/image.png",
  );
});

test("default upload and a teardown delete stay bound to the original API base", async () => {
  const response = deferred<Response>();
  const fetchMock = vi
    .fn()
    .mockImplementationOnce(() => response.promise)
    .mockResolvedValue(new Response(null, { status: 204 }));
  vi.stubGlobal("fetch", fetchMock);
  const store = new DraftStore("/sessions/a");
  store.beginEditorSession();
  const key = store.uploadFile(new File(["bytes"], "note.txt"));
  store.setContent({ text: "", document: occurrence(key) });
  expect(fetchMock.mock.calls[0][0]).toBe(
    "/sessions/a/v1/attachments?name=note.txt",
  );
  history.replaceState(null, "", "/sessions/b/");
  response.resolve(new Response(JSON.stringify(attachment), { status: 200 }));
  await vi.waitFor(() =>
    expect(store.snapshot().uploads[0]?.status).toBe("ready"),
  );
  store.remove(key);
  await unmount(store);
  await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
  expect(fetchMock.mock.calls[1][0]).toBe(
    "/sessions/a/v1/attachments/note.txt",
  );
});

test("a successful send clears an unchanged structured draft", () => {
  const { store } = setup();
  store.setContent({ text: "", document: attachmentDocument });
  const submitted = store.snapshot();
  store.sent(submitted);
  expect(store.snapshot().document).toBeNull();
});

test("a same-label access edit during a pending send survives", () => {
  const { store } = setup();
  const target = {
    type: "filesystem" as const,
    original: "./main.tsx",
    path: "/w/main.tsx",
    target: "file" as const,
    access: { read: true, write: false },
  };
  const definition: ReferenceDefinition = { label: "main.tsx", target };
  const document: UserContent = {
    content: [{ Text: "look " }, { Reference: "r1" }],
    references: { r1: definition },
  };
  store.setContent({ text: "look ./main.tsx", document });
  const submitted = store.snapshot();
  const edited: UserContent = {
    content: document.content,
    references: {
      r1: {
        ...definition,
        target: { ...target, access: { read: true, write: true } },
      },
    },
  };
  // The readable spelling is identical; only the structure changed.
  store.setContent({ text: "look ./main.tsx", document: edited });
  store.sent(submitted);
  expect(store.snapshot().text).toBe("look ./main.tsx");
  expect(store.snapshot().document).toEqual(edited);
});
