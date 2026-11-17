// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type { ReferenceDefinition, UserContent } from "../api/types.ts";
import { Composer, type Recalled } from "./Composer.tsx";
import { acquireDraft } from "./drafts.ts";
import {
  agentDocument,
  appendToEditor,
  attachmentOnly,
  composerText,
  defaults,
  editorOf,
  insertReference,
  installStorage,
  mount,
  recallArrival,
  setEditorText,
} from "./composerTestKit.tsx";

beforeEach(installStorage);
afterEach(() => vi.unstubAllGlobals());

test("composer text and completed attachments return after switching sessions", async () => {
  const a = `draft-${crypto.randomUUID()}`;
  const b = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${a}`);
  store.setContent({
    text: "a's unsent text",
    document: {
      content: [{ Text: "a's unsent text" }, { Reference: "r1" }],
      references: {
        r1: {
          label: "note.txt",
          target: {
            type: "attachment",
            name: "note.txt",
            path: "/stored/note.txt",
            kind: { Binary: { bytes: 5 } },
          },
        },
      },
    },
  });
  const view = mount();
  try {
    view.render(a);
    await act(async () => {});
    expect(composerText(view.host)).toBe("a's unsent text");
    expect(view.host.querySelector(".reference-chip")?.textContent).toContain(
      "note.txt",
    );
    view.render(b);
    expect(composerText(view.host)).toBe("");
    expect(view.host.querySelector(".reference-chip")).toBeNull();
    view.render(a);
    await act(async () => {});
    expect(composerText(view.host)).toBe("a's unsent text");
    expect(view.host.querySelector(".reference-chip")?.textContent).toContain(
      "note.txt",
    );
  } finally {
    view.close();
  }
});

test("switching sessions gives a fresh editor, not the last one's undo stack", async () => {
  const a = `draft-${crypto.randomUUID()}`;
  const b = `draft-${crypto.randomUUID()}`;
  const definition: ReferenceDefinition = {
    label: "@castback",
    target: { type: "agent", session: "castback" },
  };
  const view = mount();
  try {
    view.render(a);
    setEditorText(view.host, "a text");
    insertReference(view.host, "a1", definition);
    expect(acquireDraft(`/sessions/${a}`).snapshot().document).not.toBeNull();
    // React node views render on a deferred tick.
    await act(async () => {});
    expect(view.host.querySelectorAll(".reference-chip")).toHaveLength(1);
    view.render(b);
    appendToEditor(view.host, "b text");
    act(() => editorOf(view.host).commands.undo());
    expect(view.host.querySelectorAll(".reference-chip")).toHaveLength(0);
    expect(acquireDraft(`/sessions/${b}`).snapshot().text).not.toContain(
      "a text",
    );
  } finally {
    view.close();
  }
});

test("failed sends retain a draft and successful sends clear it", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  store.setText("keep until accepted");
  const send = vi
    .fn()
    .mockRejectedValueOnce(new Error("offline"))
    .mockResolvedValueOnce(undefined);
  const view = mount(send);
  try {
    view.render(id);
    await act(async () => {
      view.host
        .querySelector("form")
        ?.dispatchEvent(
          new Event("submit", { bubbles: true, cancelable: true }),
        );
    });
    expect(store.snapshot().text).toBe("keep until accepted");
    expect(view.host.querySelector("[role=alert]")?.textContent).toBe(
      "offline",
    );
    expect(composerText(view.host)).toBe("keep until accepted");
    await act(async () => {
      view.host
        .querySelector("form")
        ?.dispatchEvent(
          new Event("submit", { bubbles: true, cancelable: true }),
        );
    });
    expect(store.snapshot().text).toBe("");
    expect(composerText(view.host)).toBe("");
  } finally {
    view.close();
  }
});

test("a plain send keeps the trimmed path and exact multiline storage", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const send = vi.fn(() => Promise.resolve());
  const view = mount(send);
  try {
    view.render(id);
    setEditorText(view.host, "  spaced  \n\n");
    expect(acquireDraft(`/sessions/${id}`).snapshot().text).toBe(
      "  spaced  \n\n",
    );
    await act(async () =>
      view.host.querySelector<HTMLFormElement>("form")!.requestSubmit(),
    );
    expect(send).toHaveBeenCalledExactlyOnceWith("spaced", [], null);
  } finally {
    view.close();
  }
});

test("a send accepted after switching does not clear either session's newer draft", async () => {
  const a = `draft-${crypto.randomUUID()}`;
  const b = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${a}`);
  store.setText("first turn");
  let resolve!: () => void;
  const pending = new Promise<void>((yes) => {
    resolve = yes;
  });
  const send = vi.fn(() => pending);
  const view = mount(send);
  try {
    view.render(a);
    act(() =>
      view.host.querySelector<HTMLFormElement>("form")!.requestSubmit(),
    );
    setEditorText(view.host, "next turn");
    view.render(b);
    setEditorText(view.host, "b draft");
    await act(async () => {
      resolve();
      await pending;
    });
    expect(store.snapshot().text).toBe("next turn");
    expect(composerText(view.host)).toBe("b draft");
    view.render(a);
    expect(composerText(view.host)).toBe("next turn");
  } finally {
    view.close();
  }
});
test("a send finishing after a session switch leaves the new session's draft alone", async () => {
  const a = `draft-${crypto.randomUUID()}`;
  const b = `draft-${crypto.randomUUID()}`;
  const aStore = acquireDraft(`/sessions/${a}`);
  aStore.setText("first turn");
  const bStore = acquireDraft(`/sessions/${b}`);
  bStore.setText("b draft");
  let resolve!: () => void;
  const pending = new Promise<void>((yes) => {
    resolve = yes;
  });
  const send = vi.fn(() => pending);
  // Production keeps one composer across a session switch; the editor inside it
  // is remounted by base. An unkeyed mount is what lets the old request's
  // editor ref point at the new session.
  const view = mount(send, { keyed: false });
  try {
    view.render(a);
    act(() =>
      view.host.querySelector<HTMLFormElement>("form")!.requestSubmit(),
    );
    view.render(b);
    expect(composerText(view.host)).toBe("b draft");
    await act(async () => {
      resolve();
      await pending;
    });
    // The accepted send still clears the session it belonged to, but the
    // session now on screen keeps both its store and its editor content.
    expect(aStore.snapshot().text).toBe("");
    expect(bStore.snapshot().text).toBe("b draft");
    expect(bStore.snapshot().document).toBeNull();
    expect(composerText(view.host)).toBe("b draft");
  } finally {
    view.close();
  }
});

test("composer hold sends a loop patch without submitting or clearing its draft", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  store.setText("keep this draft while holding");
  const fetch = vi.fn(
    async () =>
      new Response(
        JSON.stringify({
          pace: { held: true, speed_ms: 0 },
          review_level: "spotter_human",
          loop_phase: { phase: "idle" },
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      ),
  );
  vi.stubGlobal("fetch", fetch);
  const send = vi.fn(() => Promise.resolve());
  const view = mount(send);
  try {
    view.render(id);
    await act(async () =>
      view.host.querySelector<HTMLButtonElement>(".hold-btn")!.click(),
    );
    expect(fetch).toHaveBeenCalledOnce();
    const [url, init] = fetch.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe(`/sessions/${id}/v1/loop`);
    expect(init.method).toBe("PATCH");
    expect(JSON.parse(init.body as string)).toEqual({ pace: { held: true } });
    expect(send).not.toHaveBeenCalled();
    expect(store.snapshot().text).toBe("keep this draft while holding");
  } finally {
    view.close();
  }
});

test("an empty send the verbs topic refuses states the reason without a request", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const fetch = vi.fn();
  vi.stubGlobal("fetch", fetch);
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  history.replaceState(null, "", `/sessions/${id}/`);
  const verbs = {
    turn: { accepted: true, delivery: "now" },
    continue: {
      accepted: false,
      message: "nothing to continue: the last turn completed",
    },
    compact: { accepted: true, warm: true },
  } as const;
  try {
    act(() =>
      root.render(
        <Composer
          key={id}
          {...defaults}
          phase={{ phase: "idle" }}
          verbs={verbs}
          submit={vi.fn(() => Promise.resolve())}
        />,
      ),
    );
    await act(async () =>
      host.querySelector<HTMLFormElement>("form")!.requestSubmit(),
    );
    expect(fetch).not.toHaveBeenCalled();
    expect(host.querySelector("[role=alert]")?.textContent).toBe(
      "nothing to continue: the last turn completed",
    );
  } finally {
    act(() => root.unmount());
    host.remove();
  }
});

test("desktop Enter sends, Shift-Enter and a composing Enter do not", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const send = vi.fn(() => Promise.resolve());
  const view = mount(send);
  try {
    view.render(id);
    setEditorText(view.host, "send me");
    const editor = view.host.querySelector(".composer-editor")!;
    act(() =>
      editor.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "Enter",
          shiftKey: true,
          bubbles: true,
          cancelable: true,
        }),
      ),
    );
    expect(send).not.toHaveBeenCalled();
    act(() =>
      editor.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "Enter",
          isComposing: true,
          bubbles: true,
          cancelable: true,
        }),
      ),
    );
    expect(send).not.toHaveBeenCalled();
    await act(async () =>
      editor.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "Enter",
          bubbles: true,
          cancelable: true,
        }),
      ),
    );
    expect(send).toHaveBeenCalledExactlyOnceWith("send me", [], null);
  } finally {
    view.close();
  }
});

test("a second recall keeps an attachment-only document inline and unstaged", () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  const view = mount();
  try {
    view.render(id, {
      text: "",
      attachments: [recallArrival("shot.png")],
      document: attachmentOnly(),
      nonce: 1,
    });
    expect(store.snapshot().text).toBe("");
    expect(store.snapshot().document).not.toBeNull();
    expect(store.snapshot().uploads).toEqual([]);
    expect(view.host.querySelectorAll(".reference-chip")).toHaveLength(1);
    view.render(id, {
      text: "more",
      attachments: [],
      document: null,
      nonce: 2,
    });
    expect(store.snapshot().text).toBe("more");
    expect(Object.keys(store.snapshot().document!.references)).toHaveLength(1);
    expect(view.host.querySelectorAll(".reference-chip")).toHaveLength(1);
  } finally {
    view.close();
  }
});
test("removing a recalled attachment chip does not delete its sent upload", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  const fetch = vi.fn(() =>
    Promise.resolve(new Response(null, { status: 204 })),
  );
  vi.stubGlobal("fetch", fetch);
  const view = mount();
  try {
    view.render(id, {
      text: "",
      attachments: [recallArrival("shot.png")],
      document: attachmentOnly(),
      nonce: 1,
    });
    await act(async () => {});
    expect(view.host.querySelectorAll(".reference-chip")).toHaveLength(1);
    await act(async () => {
      view.host
        .querySelector<HTMLButtonElement>(".reference-chip-remove")!
        .click();
    });
    await act(async () => {});
    expect(fetch).not.toHaveBeenCalled();
    expect(store.snapshot().document).toBeNull();
    expect(view.host.querySelectorAll(".reference-chip")).toHaveLength(0);
    // The upload is still stored, so undo can put the chip back.
    act(() => editorOf(view.host).commands.undo());
    await act(async () => {});
    expect(fetch).not.toHaveBeenCalled();
    expect(view.host.querySelectorAll(".reference-chip")).toHaveLength(1);
    expect(Object.keys(store.snapshot().document!.references)).toHaveLength(1);
  } finally {
    view.close();
  }
});

test("the editor's HTML export carries a readable label, not reference identity", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const definition: ReferenceDefinition = {
    label: "main.tsx",
    target: {
      type: "filesystem",
      original: "./main.tsx",
      path: "/w/main.tsx",
      target: "file",
      access: { read: true, write: false },
    },
  };
  const view = mount();
  try {
    view.render(id);
    insertReference(view.host, "r1", definition);
    await act(async () => {});
    const html = editorOf(view.host).getHTML();
    expect(html).toContain("main.tsx");
    expect(html).not.toContain("data-efference-definition");
    expect(html).not.toContain("definition=");
    expect(html).not.toContain('id="');
  } finally {
    view.close();
  }
});

test("an edited recall stays editable and resends its merged document", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  const send = vi.fn(() => Promise.resolve());
  const view = mount(send);
  try {
    view.render(id, {
      text: "look at @castback please",
      attachments: [recallArrival("shot.png")],
      document: agentDocument(),
      nonce: 1,
    });
    expect(store.snapshot().text).toBe("look at @castback please");
    expect(store.snapshot().document).not.toBeNull();
    appendToEditor(view.host, " edited");
    expect(store.snapshot().text).toBe("look at @castback please edited");
    view.render(id, {
      text: "more",
      attachments: [],
      document: null,
      nonce: 2,
    });
    expect(store.snapshot().text).toBe("more\nlook at @castback please edited");
    expect(view.host.querySelector("[role=alert]")).toBeNull();
    expect(Object.keys(store.snapshot().document!.references).sort()).toEqual([
      "a",
      "b",
    ]);
    await act(async () =>
      view.host.querySelector<HTMLFormElement>("form")!.requestSubmit(),
    );
    const sent = send.mock.calls[0] as unknown as [
      string,
      unknown,
      NonNullable<Recalled["document"]>,
    ];
    expect(sent[0]).toBe("more\nlook at @castback please edited");
    expect(Object.keys(sent[2].references).sort()).toEqual(["a", "b"]);
  } finally {
    view.close();
  }
});

test("a successful structured send clears its document, so the next text is ordinary", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  const send = vi.fn(() => Promise.resolve());
  const view = mount(send);
  try {
    view.render(id, {
      text: "",
      attachments: [recallArrival("shot.png")],
      document: attachmentOnly(),
      nonce: 1,
    });
    await act(async () =>
      view.host.querySelector<HTMLFormElement>("form")!.requestSubmit(),
    );
    const first = send.mock.calls[0] as unknown as [
      string,
      unknown,
      NonNullable<Recalled["document"]>,
    ];
    expect(Object.keys(first[2].references)).toHaveLength(1);
    expect(store.snapshot().document).toBeNull();
    expect(view.host.querySelectorAll(".reference-chip")).toHaveLength(0);
    setEditorText(view.host, "plain");
    await act(async () =>
      view.host.querySelector<HTMLFormElement>("form")!.requestSubmit(),
    );
    expect(send.mock.calls[1]).toEqual(["plain", [], null]);
  } finally {
    view.close();
  }
});

test("a same-label access edit during a pending send survives", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  const target = {
    type: "filesystem" as const,
    original: "./main.tsx",
    path: "/w/main.tsx",
    target: "file" as const,
    access: { read: true, write: false },
  };
  const document: UserContent = {
    content: [{ Text: "look " }, { Reference: "r1" }],
    references: { r1: { label: "main.tsx", target } },
  };
  store.setContent({ text: "look ./main.tsx", document });
  let resolve!: () => void;
  const pending = new Promise<void>((yes) => {
    resolve = yes;
  });
  const send = vi.fn(() => pending);
  const view = mount(send);
  try {
    view.render(id);
    await act(async () => {});
    expect(view.host.querySelector(".reference-chip-access")).not.toBeNull();
    act(() =>
      view.host.querySelector<HTMLFormElement>("form")!.requestSubmit(),
    );
    await act(async () =>
      view.host
        .querySelector<HTMLButtonElement>(".reference-chip-access")!
        .click(),
    );
    expect(store.snapshot().text).toBe("look ./main.tsx");
    await act(async () => {
      resolve();
      await pending;
    });
    const kept = store.snapshot().document!;
    expect(kept.references.r1?.target).toEqual({
      ...target,
      access: { read: true, write: true },
    });
    expect(store.snapshot().text).toBe("look ./main.tsx");
  } finally {
    view.close();
  }
});

test("copy and paste preserve identity in the same session", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  const definition: ReferenceDefinition = {
    label: "main.tsx",
    target: {
      type: "filesystem",
      original: "./main.tsx",
      path: "/w/main.tsx",
      target: "file",
      access: { read: true, write: false },
    },
  };
  store.setContent({
    text: "look ./main.tsx",
    document: {
      content: [{ Text: "look " }, { Reference: "r1" }],
      references: { r1: definition },
    },
  });
  const values = new Map<string, string>();
  const clipboard = (type: "copy" | "cut" | "paste") => {
    const event = new Event(type, {
      bubbles: true,
      cancelable: true,
    });
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
    act(() => editorOf(view.host).chain().focus().selectAll().run());
    view.host
      .querySelector(".composer-editor")!
      .dispatchEvent(clipboard("copy"));
    expect(values.get("text/plain")).toBe("look ./main.tsx");
    expect(values.has("text/html")).toBe(true);
    expect(values.has("application/x-efference-references+json")).toBe(true);
    setEditorText(view.host, "");
    await act(async () => {
      view.host
        .querySelector(".composer-editor")!
        .dispatchEvent(clipboard("paste"));
    });
    expect(store.snapshot().document).not.toBeNull();
    expect(view.host.querySelectorAll(".reference-chip")).toHaveLength(1);
    expect(store.snapshot().text).toBe("look ./main.tsx");
    expect(Object.keys(store.snapshot().document!.references)).toHaveLength(1);
  } finally {
    view.close();
  }
});

test("a token copied in one session pastes as text in another", () => {
  const a = `draft-${crypto.randomUUID()}`;
  const b = `draft-${crypto.randomUUID()}`;
  const definition: ReferenceDefinition = {
    label: "@castback",
    target: { type: "agent", session: "castback" },
  };
  const store = acquireDraft(`/sessions/${a}`);
  store.setContent({
    text: "@castback",
    document: {
      content: [{ Reference: "a1" }],
      references: { a1: definition },
    },
  });
  const values = new Map<string, string>();
  const clipboard = (type: "copy" | "paste") => {
    const event = new Event(type, {
      bubbles: true,
      cancelable: true,
    });
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
    view.render(a);
    act(() => editorOf(view.host).chain().focus().selectAll().run());
    view.host
      .querySelector(".composer-editor")!
      .dispatchEvent(clipboard("copy"));
    view.render(b);
    act(() =>
      view.host
        .querySelector(".composer-editor")!
        .dispatchEvent(clipboard("paste")),
    );
    expect(view.host.querySelectorAll(".reference-chip")).toHaveLength(0);
    expect(acquireDraft(`/sessions/${b}`).snapshot().text).toBe("@castback");
  } finally {
    view.close();
  }
});

test("cut copies the token before removing the reference", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  store.setContent({
    text: "look ./main.tsx",
    document: {
      content: [{ Text: "look " }, { Reference: "r1" }],
      references: {
        r1: {
          label: "main.tsx",
          target: {
            type: "filesystem",
            original: "./main.tsx",
            path: "/w/main.tsx",
            target: "file",
            access: { read: true, write: false },
          },
        },
      },
    },
  });
  const values = new Map<string, string>();
  const clipboard = (type: "cut" | "paste") => {
    const event = new Event(type, {
      bubbles: true,
      cancelable: true,
    });
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
    act(() => editorOf(view.host).chain().focus().selectAll().run());
    act(() =>
      view.host
        .querySelector(".composer-editor")!
        .dispatchEvent(clipboard("cut")),
    );
    expect(view.host.querySelectorAll(".reference-chip")).toHaveLength(0);
    expect(store.snapshot().text).toBe("");
    expect(store.snapshot().document).toBeNull();
    await act(async () => {
      view.host
        .querySelector(".composer-editor")!
        .dispatchEvent(clipboard("paste"));
    });
    expect(view.host.querySelectorAll(".reference-chip")).toHaveLength(1);
    expect(store.snapshot().text).toBe("look ./main.tsx");
    expect(Object.keys(store.snapshot().document!.references)).toHaveLength(1);
  } finally {
    view.close();
  }
});
