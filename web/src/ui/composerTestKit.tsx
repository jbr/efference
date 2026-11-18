// The harness shared by the Composer tests (Composer.test.tsx and
// ComposerUpload.test.tsx): a real Tiptap editor mounted through the component,
// jsdom's missing layout, and the events a test drives paste, copy and drop
// with.

import { act, type ComponentProps } from "react";
import { createRoot } from "react-dom/client";
import { vi } from "vitest";
import { textDocument } from "../api/document.ts";
import type { Arrival, ReferenceDefinition } from "../api/types.ts";
import { editorContentFromDraft } from "./composerCore.tsx";
import { Composer, type Recalled } from "./Composer.tsx";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
// jsdom implements no layout. ProseMirror's scroll-into-view measures the DOM
// through getClientRects/getBoundingClientRect, which jsdom omits on text
// nodes and ranges; without them a `focus()` rAF throws outside any test's
// assertions.
if (!("getClientRects" in Node.prototype)) {
  const noRect = () => ({
    x: 0,
    y: 0,
    top: 0,
    left: 0,
    right: 0,
    bottom: 0,
    width: 0,
    height: 0,
    toJSON: () => ({}),
  });
  for (const target of [Node.prototype, Range.prototype]) {
    Object.assign(target as unknown as Record<string, unknown>, {
      getClientRects: () => [],
      getBoundingClientRect: noRect,
    });
  }
}

/** Empty origin storage; the draft store is a module singleton per API base,
 * so every test starts from a fresh map. */
export function installStorage() {
  const values = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => void values.set(key, value),
    removeItem: (key: string) => void values.delete(key),
  });
}

export const defaults = {
  recalled: null,
  pace: null,
  phase: null,
  verbs: null,
  stall: null,
  retrying: false,
  gatePending: false,
  ask: null,
  cold: null,
  compactionFailure: null,
  operations: null,
  model: "test",
};
export function mount(
  send: ComponentProps<typeof Composer>["submit"] = vi.fn(() =>
    Promise.resolve(),
  ),
  { keyed = true }: { keyed?: boolean } = {},
) {
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const render = (id: string, recalled: Recalled | null = null) => {
    history.replaceState(null, "", `/sessions/${id}/`);
    act(() =>
      root.render(
        keyed ? (
          <Composer key={id} {...defaults} recalled={recalled} submit={send} />
        ) : (
          <Composer {...defaults} recalled={recalled} submit={send} />
        ),
      ),
    );
  };
  const close = () => {
    act(() => root.unmount());
    host.remove();
  };
  return { host, render, close };
}
/** A stored attachment a recall hands back by name. */
export const recallArrival = (name: string) => ({
  name,
  path: `att/composer/${name}`,
  kind: { Binary: { bytes: 3 } },
});

export const agentDocument = (): NonNullable<Recalled["document"]> => ({
  content: [
    { Text: "look at " },
    { Reference: "a" },
    { Text: " please" },
    { Reference: "b" },
  ],
  references: {
    a: { label: "reviewer", target: { type: "agent", session: "castback" } },
    b: {
      label: "shot.png",
      target: { type: "attachment", ...recallArrival("shot.png") },
    },
  },
});

export const attachmentOnly = (): NonNullable<Recalled["document"]> => ({
  content: [{ Reference: "r1" }],
  references: {
    r1: {
      label: "shot.png",
      target: { type: "attachment", ...recallArrival("shot.png") },
    },
  },
});

type Chain = {
  focus: (position?: "end" | "start") => Chain;
  insertContent: (content: unknown) => Chain;
  selectAll: () => Chain;
  run: () => void;
};
export interface LiveEditor {
  commands: {
    setContent: (content: unknown, options?: { emitUpdate?: boolean }) => void;
    undo: () => void;
    redo: () => void;
  };
  getHTML: () => string;
  chain: () => Chain;
}
export const editorOf = (host: HTMLElement): LiveEditor => {
  const element = host.querySelector<HTMLElement & { editor?: LiveEditor }>(
    ".composer-editor",
  );
  if (!element?.editor) throw new Error("composer editor is not mounted");
  return element.editor;
};
export const setEditorText = (host: HTMLElement, text: string) => {
  act(() =>
    editorOf(host).commands.setContent(
      editorContentFromDraft(textDocument(text)),
    ),
  );
};
export const appendToEditor = (host: HTMLElement, text: string) => {
  act(() =>
    editorOf(host)
      .chain()
      .focus("end")
      .insertContent({ type: "text", text })
      .run(),
  );
};
/** The editor's readable text: a reference or upload chip's label is rendered
 * inside its paragraph but spells nothing in the draft, so the chip's own
 * chrome is removed before reading. */
export const composerText = (host: HTMLElement): string =>
  [...host.querySelectorAll(".composer-editor p")]
    .map((paragraph) => {
      const copy = paragraph.cloneNode(true) as HTMLElement;
      for (const chip of copy.querySelectorAll(".reference-chip"))
        chip.remove();
      for (const line of copy.querySelectorAll("br")) {
        if (line.classList.contains("ProseMirror-trailingBreak")) line.remove();
        else line.replaceWith("\n");
      }
      return copy.textContent ?? "";
    })
    .join("\n");
export const insertReference = (
  host: HTMLElement,
  id: string,
  definition: ReferenceDefinition,
) => {
  act(() =>
    editorOf(host)
      .chain()
      .focus("end")
      .insertContent({ type: "reference", attrs: { id, definition } })
      .run(),
  );
};

/** An upload's lifecycle inside the real editor: the occurrence is an editor
 * node, its status is a store event, and the projection to the wire is what
 * blocks or admits the send. */
export const uploadArrival: Arrival = {
  name: "note.txt",
  path: "/att/composer/note.txt",
  kind: { Binary: { bytes: 5 } },
};
const uploadResponse = () =>
  new Response(JSON.stringify(uploadArrival), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
/** A fetch that records every request and answers an upload as the daemon
 * would; `land` is called by the test when the request should finish. */
export const uploadFetch = () => {
  const waiting: ((response: Response) => void)[] = [];
  const fetch = vi.fn((_url: string, init?: RequestInit) =>
    init?.method === "DELETE"
      ? Promise.resolve(new Response(null, { status: 204 }))
      : new Promise<Response>((resolve) => waiting.push(resolve)),
  );
  const land = () => {
    for (const resolve of waiting.splice(0)) resolve(uploadResponse());
  };
  vi.stubGlobal("fetch", fetch);
  return { fetch, land };
};
export const pasteFiles = (host: HTMLElement, files: File[]) => {
  const event = new Event("paste", { bubbles: true, cancelable: true });
  Object.defineProperty(event, "clipboardData", {
    value: { files, getData: () => "" },
  });
  act(() => {
    host.querySelector(".composer-editor")!.dispatchEvent(event);
  });
};
export const pasteText = (host: HTMLElement, text: string) => {
  const event = new Event("paste", { bubbles: true, cancelable: true });
  Object.defineProperty(event, "clipboardData", {
    value: {
      files: [],
      getData: (kind: string) => (kind === "text/plain" ? text : ""),
    },
  });
  act(() => {
    host.querySelector(".composer-editor")!.dispatchEvent(event);
  });
};
export const uploadChip = (host: HTMLElement) =>
  host.querySelector<HTMLElement>(".upload-chip");
export const submitForm = (host: HTMLElement) =>
  act(async () => host.querySelector<HTMLFormElement>("form")!.requestSubmit());
