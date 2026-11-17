// Fixtures shared by the draft store's tests: the stored arrivals a document
// can name, the deferred upload a test lands by hand, and the browser stubs a
// draft needs.

import { vi } from "vitest";
import type {
  Arrival,
  ReferenceDefinition,
  UserContent,
} from "../../api/types.ts";
import { DraftStore } from "../drafts.ts";
import type { DraftContent } from "../draftDocument.ts";

export const attachment: Arrival = {
  name: "note.txt",
  path: "/stored/note.txt",
  kind: { Binary: { bytes: 5 } },
};
export const attachmentDefinition: ReferenceDefinition = {
  label: "note.txt",
  target: { type: "attachment", ...attachment },
};
export const attachmentDocument: UserContent = {
  content: [{ Reference: "r1" }],
  references: { r1: attachmentDefinition },
};
export const image: Arrival = {
  name: "image.png",
  path: "/stored/image.png",
  kind: { Image: { format: "PNG", width: 1, height: 1, bytes: 5 } },
};
export const occurrence = (key: string): DraftContent => ({
  content: [{ Upload: key }],
  references: {},
});
export function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
export function setup(base = "/sessions/a") {
  const pending = deferred<Arrival>();
  const upload = vi.fn(() => pending.promise);
  const remove = vi.fn(() => Promise.resolve());
  const store = new DraftStore(base, { storage: localStorage, upload, remove });
  return { store, upload, remove, pending };
}
/** The editor is torn down, so the pruner's microtask can run. */
export async function unmount(store: DraftStore) {
  store.endEditorSession();
  await Promise.resolve();
  await Promise.resolve();
}
/** Empty origin storage, a session URL, and object URLs that count their
 * revocation. */
export function installBrowser() {
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
}
