// @vitest-environment jsdom
import { act } from "react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { EditorView } from "@tiptap/pm/view";
import { acquireDraft } from "./drafts.ts";
import {
  appendToEditor,
  attachmentOnly,
  composerText,
  editorOf,
  installStorage,
  mount,
  pasteFiles,
  pasteText,
  recallArrival,
  setEditorText,
  submitForm,
  uploadChip,
  uploadFetch,
} from "./composerTestKit.tsx";

beforeEach(installStorage);
afterEach(() => vi.unstubAllGlobals());

// A staged upload's lifecycle inside the real editor: the occurrence is an
// editor node, its status is a store event, and the projection to the wire is
// what blocks or admits the send.
test("a pending upload blocks the send and names the file", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  const { land } = uploadFetch();
  const send = vi.fn(() => Promise.resolve());
  const view = mount(send);
  try {
    view.render(id);
    pasteFiles(view.host, [new File(["bytes"], "note.txt")]);
    await act(async () => {});
    expect(store.snapshot().uploads[0]).toMatchObject({ status: "uploading" });
    await submitForm(view.host);
    expect(send).not.toHaveBeenCalled();
    const alert = view.host.querySelector("[role=alert]")?.textContent ?? "";
    expect(alert).toContain("note.txt");
    // Only a staged paste has text to insert as text; a file is reattached.
    expect(alert).toContain("remove it, then send");
    expect(alert).not.toContain("insert it as text");
    // The occurrence is still in the editor: nothing was dropped behind it.
    expect(uploadChip(view.host)?.className).toContain("uploading");
    land();
  } finally {
    view.close();
  }
});

test("a failed upload blocks the send with the daemon's reason", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  vi.stubGlobal(
    "fetch",
    vi.fn(() => Promise.reject(new Error("offline"))),
  );
  const send = vi.fn(() => Promise.resolve());
  const view = mount(send);
  try {
    view.render(id);
    pasteFiles(view.host, [new File(["bytes"], "note.txt")]);
    await act(async () => {});
    expect(store.snapshot().uploads[0]?.status).toBe("failed");
    await submitForm(view.host);
    expect(send).not.toHaveBeenCalled();
    const alert = view.host.querySelector("[role=alert]")?.textContent ?? "";
    expect(alert).toContain("note.txt");
    expect(alert).toContain("offline");
    expect(alert).toContain("remove it, then send");
  } finally {
    view.close();
  }
});

test("an upload landing updates its chip without replacing the editor content", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  const { land } = uploadFetch();
  const view = mount();
  try {
    view.render(id);
    setEditorText(view.host, "look ");
    appendToEditor(view.host, "here");
    act(() => editorOf(view.host).chain().focus("end").run());
    pasteFiles(view.host, [new File(["bytes"], "note.txt")]);
    await act(async () => {});
    const before = editorOf(view.host).getHTML();
    land();
    await act(async () => {});
    expect(store.snapshot().uploads[0]?.status).toBe("ready");
    // The node view reads the store; the editor document is untouched.
    expect(editorOf(view.host).getHTML()).toBe(before);
    expect(uploadChip(view.host)?.className).toContain("ready");
    expect(store.snapshot().text).toBe("look here");
    expect(store.snapshot().document?.content).toContainEqual({
      Upload: store.snapshot().uploads[0]!.key,
    });
  } finally {
    view.close();
  }
});

test("a long paste stays an inline occurrence whose action inserts its text", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  uploadFetch();
  const view = mount();
  try {
    const text = Array.from({ length: 20 }, (_, i) => `line ${i}`).join("\n");
    view.render(id);
    pasteText(view.host, text);
    await act(async () => {});
    expect(store.snapshot().uploads).toHaveLength(1);
    expect(store.snapshot().uploads[0]?.label).toBe("paste.txt");
    const inline = view.host.querySelector<HTMLButtonElement>(".upload-inline");
    expect(inline).not.toBeNull();
    await submitForm(view.host);
    expect(view.host.querySelector("[role=alert]")?.textContent).toContain(
      "remove it or insert it as text",
    );
    act(() => inline!.click());
    await act(async () => {});
    // The record is detached, not deleted: undo can put the chip back while
    // the editor's history exists.
    expect(store.snapshot().uploads).toHaveLength(1);
    expect(store.snapshot().document).toBeNull();
    expect(composerText(view.host)).toBe(text);
  } finally {
    view.close();
  }
  await act(async () => {
    // Tiptap defers editor.destroy() by a timer, so onDestroy (and the prune
    // it triggers) lands after the unmount.
    await new Promise((resolve) => setTimeout(resolve, 5));
  });
  // No history is left to restore the chip, so the never-sent record goes.
  expect(store.snapshot().uploads).toEqual([]);
});

test("removing an upload chip during a pending send keeps the chip gone", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  const { fetch, land } = uploadFetch();
  let finishSend!: () => void;
  const sending = new Promise<void>((yes) => {
    finishSend = yes;
  });
  const send = vi.fn(() => sending);
  const view = mount(send);
  try {
    view.render(id);
    pasteFiles(view.host, [new File(["bytes"], "note.txt")]);
    await act(async () => {});
    land();
    await act(async () => {});
    act(() =>
      view.host.querySelector<HTMLFormElement>("form")!.requestSubmit(),
    );
    expect(send).toHaveBeenCalledOnce();
    // Removal is a draft edit; the pin holds the bytes, not the chip.
    act(() =>
      view.host
        .querySelector<HTMLButtonElement>(
          ".upload-chip .reference-chip-remove",
        )!
        .click(),
    );
    await act(async () => {});
    expect(uploadChip(view.host)).toBeNull();
    await act(async () => {
      finishSend();
      await sending;
    });
    expect(uploadChip(view.host)).toBeNull();
    // A completed upload the send named is retained, never deleted.
    expect(
      fetch.mock.calls.filter(([, init]) => init?.method === "DELETE"),
    ).toEqual([]);
    expect(store.snapshot().document).toBeNull();
    expect(store.snapshot().uploads).toEqual([]);
  } finally {
    view.close();
  }
});

test("a cleared send is not undoable back into its uploads", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  const { land } = uploadFetch();
  const send = vi.fn(() => Promise.resolve());
  const view = mount(send);
  try {
    view.render(id);
    pasteFiles(view.host, [new File(["bytes"], "note.txt")]);
    await act(async () => {});
    land();
    await act(async () => {});
    await submitForm(view.host);
    expect(store.snapshot().document).toBeNull();
    expect(composerText(view.host)).toBe("");
    // The cleared draft starts a fresh editor: the send is not in its history.
    act(() => editorOf(view.host).commands.undo());
    await act(async () => {});
    expect(uploadChip(view.host)).toBeNull();
    expect(store.snapshot().document).toBeNull();
    expect(store.snapshot().uploads).toEqual([]);
  } finally {
    view.close();
  }
});

test("repeated sends do not accumulate upload records", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  const { land } = uploadFetch();
  const send = vi.fn(() => Promise.resolve());
  const view = mount(send);
  try {
    view.render(id);
    for (let round = 0; round < 2; round += 1) {
      pasteFiles(view.host, [new File(["bytes"], "note.txt")]);
      await act(async () => {});
      land();
      await act(async () => {});
      await submitForm(view.host);
      await act(async () => {});
    }
    expect(send).toHaveBeenCalledTimes(2);
    expect(store.snapshot().uploads).toEqual([]);
  } finally {
    view.close();
  }
});

test("an upload occurrence copies as its file name, not a second identity", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  const { land } = uploadFetch();
  const values = new Map<string, string>();
  const clipboard = (type: "copy" | "paste") => {
    const event = new Event(type, { bubbles: true, cancelable: true });
    Object.defineProperty(event, "clipboardData", {
      value: {
        files: [],
        setData: (kind: string, value: string) => values.set(kind, value),
        getData: (kind: string) => values.get(kind) ?? "",
      },
    });
    return event;
  };
  const view = mount();
  try {
    view.render(id);
    pasteFiles(view.host, [new File(["bytes"], "note.txt")]);
    await act(async () => {});
    land();
    await act(async () => {});
    act(() => editorOf(view.host).chain().focus().selectAll().run());
    act(() =>
      view.host
        .querySelector(".composer-editor")!
        .dispatchEvent(clipboard("copy")),
    );
    expect(values.get("text/plain")).toBe("note.txt");
    expect(values.get("application/x-efference-references+json")).toBe(
      JSON.stringify({ content: [{ Text: "note.txt" }], tokens: {} }),
    );
    setEditorText(view.host, "");
    await act(async () => {
      view.host
        .querySelector(".composer-editor")!
        .dispatchEvent(clipboard("paste"));
    });
    expect(uploadChip(view.host)).toBeNull();
    expect(store.snapshot().text).toBe("note.txt");
  } finally {
    view.close();
  }
});

test("a recalled attachment chip copies as its readable name, not a token", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  store.setContent({
    text: "look ",
    document: {
      content: [{ Text: "look " }, { Reference: "r1" }],
      references: attachmentOnly().references,
    },
  });
  const values = new Map<string, string>();
  const event = new Event("copy", { bubbles: true, cancelable: true });
  Object.defineProperty(event, "clipboardData", {
    value: {
      files: [],
      setData: (kind: string, value: string) => values.set(kind, value),
      getData: (kind: string) => values.get(kind) ?? "",
    },
  });
  const view = mount();
  try {
    view.render(id);
    await act(async () => {});
    act(() => editorOf(view.host).chain().focus().selectAll().run());
    act(() =>
      view.host.querySelector(".composer-editor")!.dispatchEvent(event),
    );
    expect(values.get("text/plain")).toBe("look shot.png");
    expect(values.get("application/x-efference-references+json")).toBe(
      JSON.stringify({
        content: [{ Text: "look " }, { Text: "shot.png" }],
        tokens: {},
      }),
    );
  } finally {
    view.close();
  }
});

test("the picker inserts at the retained caret, after the prose", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  uploadFetch();
  const view = mount();
  try {
    view.render(id);
    setEditorText(view.host, "ab");
    act(() => editorOf(view.host).chain().focus("end").run());
    const picker =
      view.host.querySelector<HTMLInputElement>('input[type="file"]')!;
    const file = new File(["bytes"], "b.txt");
    Object.defineProperty(picker, "files", {
      value: [file],
      configurable: true,
    });
    act(() => picker.dispatchEvent(new Event("change", { bubbles: true })));
    await act(async () => {});
    const content = store.snapshot().document?.content ?? [];
    const key = store.snapshot().uploads[0]!.key;
    expect(content).toEqual([{ Text: "ab" }, { Upload: key }]);
  } finally {
    view.close();
  }
});

test("a drop on the editor inserts at the pointer, once", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  uploadFetch();
  const at = vi
    .spyOn(EditorView.prototype, "posAtCoords")
    .mockReturnValue({ pos: 1, inside: -1 });
  const view = mount();
  try {
    view.render(id);
    setEditorText(view.host, "ab");
    const event = new Event("drop", { bubbles: true, cancelable: true });
    Object.defineProperty(event, "dataTransfer", {
      value: { files: [new File(["bytes"], "b.txt")], types: ["Files"] },
    });
    act(() =>
      view.host.querySelector(".composer-editor")!.dispatchEvent(event),
    );
    await act(async () => {});
    // The pointer's position (1) is before the prose; the window listener must
    // not stage the file a second time at its retained caret.
    const content = store.snapshot().document?.content ?? [];
    const key = store.snapshot().uploads[0]!.key;
    expect(content).toEqual([{ Upload: key }, { Text: "ab" }]);
    expect(store.snapshot().uploads).toHaveLength(1);
  } finally {
    at.mockRestore();
    view.close();
  }
});

test.each(["success", "failure"] as const)(
  "an upload-bearing send that settles after a session switch leaves the new session alone (%s)",
  async (outcome) => {
    const a = `draft-${crypto.randomUUID()}`;
    const b = `draft-${crypto.randomUUID()}`;
    const aStore = acquireDraft(`/sessions/${a}`);
    const bStore = acquireDraft(`/sessions/${b}`);
    bStore.setText("b draft");
    const { fetch, land } = uploadFetch();
    let settle!: () => void;
    const pending = new Promise<void>((yes, no) => {
      settle = () => (outcome === "success" ? yes() : no(new Error("offline")));
    });
    const send = vi.fn(() => pending);
    // Production keeps one composer across a session switch: the unkeyed mount
    // leaves the in-flight submit holding A's store while the editor remounts
    // against B. This is the path a keyed mount cannot exercise.
    const view = mount(send, { keyed: false });
    let outside: HTMLButtonElement | null = null;
    try {
      view.render(a);
      setEditorText(view.host, "look ");
      act(() => editorOf(view.host).chain().focus("end").run());
      pasteFiles(view.host, [new File(["bytes"], "note.txt")]);
      await act(async () => {});
      land();
      await act(async () => {});
      expect(aStore.snapshot().uploads[0]).toMatchObject({
        status: "ready",
        provenance: "draft",
      });
      act(() =>
        view.host.querySelector<HTMLFormElement>("form")!.requestSubmit(),
      );
      expect(send).toHaveBeenCalledOnce();
      // The send owns the bytes now: removing its chip is a draft edit, and the
      // pin keeps the record alive through the editor teardown that follows.
      act(() =>
        view.host
          .querySelector<HTMLButtonElement>(
            ".upload-chip .reference-chip-remove",
          )!
          .click(),
      );
      await act(async () => {});
      expect(uploadChip(view.host)).toBeNull();
      expect(aStore.snapshot().uploads[0]).toMatchObject({ pinned: true });

      // Focus outside the composer so a stolen focus after settlement is
      // observable, not masked by B's own editor already having it.
      outside = document.createElement("button");
      document.body.append(outside);
      outside.focus();
      view.render(b);
      await act(async () => {});
      // A's history is gone, but the pin keeps its detached record; B mounts
      // from its own draft.
      expect(aStore.snapshot().uploads).toHaveLength(1);
      expect(composerText(view.host)).toBe("b draft");
      const bHTML = editorOf(view.host).getHTML();
      expect(document.activeElement).toBe(outside);

      await act(async () => {
        settle();
        await pending.catch(() => {});
        // Let the post-send frame run: the success path would refocus the
        // editor from it if the base check were missing.
        await new Promise((resolve) => setTimeout(resolve, 20));
      });
      // A failed POST may still have been admitted, and a success named the
      // bytes: neither path deletes anything.
      expect(
        fetch.mock.calls.filter(([, init]) => init?.method === "DELETE"),
      ).toEqual([]);
      // The send released the pin, and no editor history is left to restore
      // the occurrence, so the retained metadata is pruned (the bytes stay).
      expect(aStore.snapshot().uploads).toEqual([]);
      // B is untouched: no error from A's settlement, no content replacement,
      // no focus theft.
      expect(view.host.querySelector("[role=alert]")).toBeNull();
      expect(bStore.snapshot().text).toBe("b draft");
      expect(bStore.snapshot().document).toBeNull();
      expect(composerText(view.host)).toBe("b draft");
      expect(editorOf(view.host).getHTML()).toBe(bHTML);
      expect(document.activeElement).toBe(outside);
    } finally {
      outside?.remove();
      view.close();
    }
  },
);

test("a failed send keeps a removed upload's bytes and leaves the chip gone", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  const { fetch, land } = uploadFetch();
  const send = vi.fn(() => Promise.reject(new Error("offline")));
  const view = mount(send);
  try {
    view.render(id);
    pasteFiles(view.host, [new File(["bytes"], "note.txt")]);
    await act(async () => {});
    land();
    await act(async () => {});
    act(() =>
      view.host.querySelector<HTMLFormElement>("form")!.requestSubmit(),
    );
    act(() =>
      view.host
        .querySelector<HTMLButtonElement>(
          ".upload-chip .reference-chip-remove",
        )!
        .click(),
    );
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 5));
    });
    expect(view.host.querySelector("[role=alert]")?.textContent).toBe(
      "offline",
    );
    expect(uploadChip(view.host)).toBeNull();
    // A failed POST may still have been admitted: the bytes stay.
    expect(
      fetch.mock.calls.filter(([, init]) => init?.method === "DELETE"),
    ).toEqual([]);
    // The record is retained while this composer lives: a failed POST may
    // still have been admitted.
    expect(store.snapshot().uploads[0]).toMatchObject({
      provenance: "sent",
      pinned: false,
    });
  } finally {
    view.close();
  }
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 5));
  });
  // No history is left to restore the chip; the metadata goes, the bytes stay.
  expect(store.snapshot().uploads).toEqual([]);
});

test("the toolbar counts only uploads the draft still shows as landing", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  const { land } = uploadFetch();
  const view = mount();
  try {
    view.render(id);
    pasteFiles(view.host, [new File(["bytes"], "note.txt")]);
    await act(async () => {});
    const sendButton = () =>
      view.host.querySelector<HTMLButtonElement>(".send-btn")!;
    expect(sendButton().disabled).toBe(true);
    // A removed chip is not visible work even while its request is still in
    // flight; only a live occurrence disables the send.
    act(() =>
      view.host
        .querySelector<HTMLButtonElement>(
          ".upload-chip .reference-chip-remove",
        )!
        .click(),
    );
    expect(store.snapshot().uploads[0]).toMatchObject({ status: "uploading" });
    expect(sendButton().disabled).toBe(false);
    land();
  } finally {
    view.close();
  }
});

test("a pinned upload in flight is not counted as landing", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  const { land } = uploadFetch();
  const send = vi.fn(() => new Promise<void>(() => {}));
  const view = mount(send);
  try {
    view.render(id);
    pasteFiles(view.host, [new File(["bytes"], "note.txt")]);
    await act(async () => {});
    land();
    await act(async () => {});
    await submitForm(view.host);
    expect(send).toHaveBeenCalledOnce();
    expect(store.snapshot().uploads[0]).toMatchObject({
      status: "ready",
      pinned: true,
    });
    expect(
      view.host.querySelector<HTMLButtonElement>(".send-btn")!.disabled,
    ).toBe(false);
  } finally {
    view.close();
  }
});

test("a file drop the editor handles clears the page's drag overlay", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  uploadFetch();
  const at = vi
    .spyOn(EditorView.prototype, "posAtCoords")
    .mockReturnValue({ pos: 1, inside: -1 });
  const view = mount();
  try {
    view.render(id);
    const files = [new File(["bytes"], "note.txt")];
    const drag = new Event("dragenter", { bubbles: true, cancelable: true });
    Object.defineProperty(drag, "dataTransfer", {
      value: { files, types: ["Files"] },
    });
    act(() => window.dispatchEvent(drag));
    expect(view.host.querySelector(".composer.dragging")).not.toBeNull();
    const drop = new Event("drop", { bubbles: true, cancelable: true });
    Object.defineProperty(drop, "dataTransfer", {
      value: { files, types: ["Files"] },
    });
    act(() => view.host.querySelector(".composer-editor")!.dispatchEvent(drop));
    await act(async () => {});
    expect(view.host.querySelector(".composer.dragging")).toBeNull();
    // The editor's own drop handler inserted it; the page listener did not
    // stage a second one.
    expect(store.snapshot().uploads).toHaveLength(1);
  } finally {
    at.mockRestore();
    view.close();
  }
});

test("a file paste replaces the selected text", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  uploadFetch();
  const view = mount();
  try {
    view.render(id);
    setEditorText(view.host, "hello world");
    act(() => editorOf(view.host).chain().focus("end").selectAll().run());
    pasteFiles(view.host, [new File(["bytes"], "note.txt")]);
    await act(async () => {});
    const key = store.snapshot().uploads[0]!.key;
    expect(store.snapshot().document?.content).toEqual([{ Upload: key }]);
    expect(composerText(view.host)).toBe("");
  } finally {
    view.close();
  }
});

test("the picker replaces the selected text and returns focus", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  uploadFetch();
  const view = mount();
  try {
    view.render(id);
    setEditorText(view.host, "hello world");
    act(() => editorOf(view.host).chain().focus("end").selectAll().run());
    const picker =
      view.host.querySelector<HTMLInputElement>('input[type="file"]')!;
    Object.defineProperty(picker, "files", {
      value: [new File(["bytes"], "note.txt")],
      configurable: true,
    });
    act(() => picker.dispatchEvent(new Event("change", { bubbles: true })));
    await act(async () => {});
    const key = store.snapshot().uploads[0]!.key;
    expect(store.snapshot().document?.content).toEqual([{ Upload: key }]);
    // Tiptap's focus command lands its DOM focus on a frame.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });
    expect(document.activeElement?.classList.contains("composer-editor")).toBe(
      true,
    );
  } finally {
    view.close();
  }
});

test("a recall of a file the draft already uploaded keeps one chip and its bytes", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  const { fetch, land } = uploadFetch();
  const view = mount();
  try {
    view.render(id);
    pasteFiles(view.host, [new File(["bytes"], "note.txt")]);
    await act(async () => {});
    land();
    await act(async () => {});
    expect(store.snapshot().uploads[0]).toMatchObject({ status: "ready" });
    // The recalled turn names the same stored file the draft's upload landed
    // as: the chip must not appear twice, and the bytes belong to the recalled
    // turn from now on.
    view.render(id, {
      text: "",
      attachments: [recallArrival("note.txt")],
      document: {
        content: [{ Reference: "r1" }],
        references: {
          r1: {
            label: "note.txt",
            target: { type: "attachment", ...recallArrival("note.txt") },
          },
        },
      },
      nonce: 1,
    });
    await act(async () => {});
    expect(view.host.querySelectorAll(".reference-chip")).toHaveLength(1);
    expect(uploadChip(view.host)).not.toBeNull();
    expect(store.snapshot().uploads[0]).toMatchObject({ provenance: "sent" });
    act(() =>
      view.host
        .querySelector<HTMLButtonElement>(
          ".upload-chip .reference-chip-remove",
        )!
        .click(),
    );
  } finally {
    view.close();
  }
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 5));
  });
  expect(
    fetch.mock.calls.filter(([, init]) => init?.method === "DELETE"),
  ).toEqual([]);
});
