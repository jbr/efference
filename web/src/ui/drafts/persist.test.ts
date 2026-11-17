// @vitest-environment jsdom
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { DraftStore } from "../drafts.ts";
import {
  attachment,
  attachmentDocument,
  installBrowser,
  occurrence,
  setup,
  unmount,
} from "./fixtures.ts";

beforeEach(installBrowser);
afterEach(() => vi.unstubAllGlobals());

test("malformed stored data is ignored without breaking the composer", () => {
  localStorage.setItem("efference.composer.v2:/sessions/a", "{");
  expect(setup().store.snapshot()).toMatchObject({
    text: "",
    uploads: [],
    document: null,
  });
});

test("an occurrence whose record was lost is visible, failed and blocks sending", () => {
  localStorage.setItem(
    "efference.composer.v2:/sessions/a",
    JSON.stringify({
      text: "",
      document: { content: [{ Upload: "lost" }], references: {} },
      uploads: [],
    }),
  );
  const snapshot = setup().store.snapshot();
  expect(snapshot.uploads).toHaveLength(1);
  expect(snapshot.uploads[0]).toMatchObject({
    key: "lost",
    status: "failed",
    provenance: "sent",
  });
  expect(snapshot.uploads[0]?.error).toContain("no longer available");
  expect(snapshot.document).toEqual({
    content: [{ Upload: "lost" }],
    references: {},
  });
});

test("denied or full storage does not break editing, uploads or content", () => {
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
  const key = store.uploadFile(new File(["bytes"], "note.txt"));
  store.setContent({ text: "", document: occurrence(key) });
  expect(store.snapshot().uploads).toHaveLength(1);
  expect(store.snapshot().document).toEqual(occurrence(key));
});

test("structured content persists and clearing the composer removes the document", () => {
  const { store } = setup();
  store.setContent({ text: "", document: attachmentDocument });
  expect(store.snapshot().document).toEqual(attachmentDocument);
  const restored = setup().store;
  expect(restored.snapshot().document).toEqual(attachmentDocument);
  // An attachment spells nothing in the readable form, so the document is the
  // only place the occurrence survives a reload.
  expect(restored.snapshot().text).toBe("");
  restored.setText("");
  expect(restored.snapshot().document).toBeNull();
});

test("exact multiline text persists without normalization", () => {
  const { store } = setup();
  store.setText("a\n\n  b  \n");
  expect(setup().store.snapshot().text).toBe("a\n\n  b  \n");
});

test("a v1 ready row becomes an inline attachment occurrence with no record", async () => {
  localStorage.setItem(
    "efference.composer.v1:/sessions/a",
    JSON.stringify({
      text: "hello",
      staged: [
        {
          key: "old",
          label: "note.txt",
          status: "ready",
          attachment,
          preview: "blob:gone",
        },
      ],
    }),
  );
  const { store, remove } = setup();
  // The v1 text field is the only copy of the prose: it leads the draft, ahead
  // of the occurrence the ready row becomes.
  expect(store.snapshot().document).toEqual({
    content: [{ Text: "hello" }, { Reference: "r1" }],
    references: attachmentDocument.references,
  });
  expect(store.snapshot().text).toBe("hello");
  expect(store.snapshot().uploads).toEqual([]);
  expect(localStorage.getItem("efference.composer.v1:/sessions/a")).toBeNull();
  expect(
    localStorage.getItem("efference.composer.v2:/sessions/a"),
  ).not.toBeNull();
  store.beginEditorSession();
  store.setContent({ text: "", document: null });
  await unmount(store);
  // A migrated ready row may have been sent; it is never deleted.
  expect(remove).not.toHaveBeenCalled();
});

test("a v1 pending row becomes a failed upload occurrence at the end, keeping its text", () => {
  localStorage.setItem(
    "efference.composer.v1:/sessions/a",
    JSON.stringify({
      text: "hello",
      staged: [
        {
          key: "old",
          label: "paste.txt",
          status: "uploading",
          pastedText: "x\ny",
        },
        { key: "bad", label: "gone.txt", status: "failed", error: "nope" },
      ],
    }),
  );
  const snapshot = setup().store.snapshot();
  expect(snapshot.document?.content).toHaveLength(3);
  const keys = snapshot.uploads.map((upload) => upload.key);
  expect(snapshot.document?.content).toEqual([
    { Text: "hello" },
    { Upload: keys[0] },
    { Upload: keys[1] },
  ]);
  expect(snapshot.text).toBe("hello");
  expect(snapshot.uploads[0]).toMatchObject({
    status: "failed",
    pastedText: "x\ny",
    provenance: "sent",
  });
  expect(snapshot.uploads[0]?.error).toContain("interrupted");
  expect(snapshot.uploads[1]).toMatchObject({
    status: "failed",
    error: "nope",
  });
});

test("a v1 plain-text draft migrates without inventing structure", () => {
  localStorage.setItem(
    "efference.composer.v1:/sessions/a",
    JSON.stringify({ text: "just prose\n", staged: [], document: null }),
  );
  const snapshot = setup().store.snapshot();
  expect(snapshot.text).toBe("just prose\n");
  expect(snapshot.document).toBeNull();
  expect(snapshot.uploads).toEqual([]);
});

test("a v1 key is kept when the v2 write fails", () => {
  localStorage.setItem(
    "efference.composer.v1:/sessions/a",
    JSON.stringify({ text: "kept", staged: [] }),
  );
  const storage = {
    getItem: (key: string) => localStorage.getItem(key),
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
  expect(store.snapshot().text).toBe("kept");
  expect(
    localStorage.getItem("efference.composer.v1:/sessions/a"),
  ).not.toBeNull();
});
