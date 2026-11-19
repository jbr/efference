// @vitest-environment jsdom
import { act } from "react";
import type { Editor } from "@tiptap/core";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { discoverAgents, discoverPaths } from "../api/discovery.ts";
import type { Arrival, PathCandidates, UserContent } from "../api/types.ts";
import { acquireDraft } from "./drafts.ts";
import {
  editorOf,
  installStorage,
  mount,
  setEditorText,
  submitForm,
} from "./composerTestKit.tsx";

vi.mock("../api/discovery.ts", () => ({
  discoverPaths: vi.fn(),
  discoverAgents: vi.fn(),
}));
const paths = (
  prefix: string,
  name = "src",
  target: "directory" | "file" = "directory",
): PathCandidates => ({
  prefix,
  directory: "/project",
  filter: "",
  truncated: false,
  entries: [
    {
      name,
      original: `${prefix.slice(0, prefix.lastIndexOf("/") + 1)}${name}`,
      path: `/project/${name}`,
      target,
      symlink: false,
    },
  ],
});
const cleanups: (() => void)[] = [];
beforeEach(() => {
  installStorage();
  vi.mocked(discoverPaths).mockImplementation(async (_base, prefix) =>
    paths(prefix),
  );
  vi.mocked(discoverAgents).mockImplementation(async (_base, prefix) => ({
    prefix,
    truncated: false,
    agents: [
      { id: "exact-agent", title: "An agent", status: "idle", processes: 0 },
    ],
  }));
});
afterEach(() => {
  cleanups.splice(0).forEach((close) => close());
  vi.clearAllMocks();
  vi.unstubAllGlobals();
});
type Submit = (
  text: string,
  attachments: Arrival[],
  document?: UserContent | null,
) => Promise<void>;
function fixture() {
  const id = `completion-${crypto.randomUUID()}`;
  const send = vi.fn<Submit>(() => Promise.resolve());
  const view = mount(send, { keyed: false });
  cleanups.push(view.close);
  view.render(id);
  return { ...view, id, send, store: acquireDraft(`/sessions/${id}`) };
}
function type(host: HTMLElement, text: string) {
  setEditorText(host, text);
  act(() => editorOf(host).chain().focus("end").run());
  act(() => host.querySelector<HTMLElement>(".composer-editor")!.focus());
}
test.each(["MacIntel", "Win32"])(
  "Ctrl-A follows composer editing conventions on %s",
  (platform) => {
    const platformGetter = vi
      .spyOn(navigator, "platform", "get")
      .mockReturnValue(platform);
    try {
      const view = fixture();
      type(view.host, "First paragraph\nSecond paragraph");
      const editor = editorOf(view.host) as unknown as Editor;
      act(() => {
        editor.commands.setHardBreak();
        editor.commands.insertContent("Last line");
      });
      const lastLineStart = editor.state.selection.head - "Last line".length;
      const paragraphStart = editor.state.selection.$head.start();
      expect(lastLineStart).toBeGreaterThan(paragraphStart);

      expect(key(view.host, "a", { ctrlKey: true }).defaultPrevented).toBe(
        true,
      );
      if (platform === "MacIntel") {
        expect(editor.state.selection.empty).toBe(true);
        expect(editor.state.selection.head).toBe(lastLineStart);
        act(() => editor.commands.setTextSelection(paragraphStart + 4));
        key(view.host, "a", { ctrlKey: true });
        expect(editor.state.selection.head).toBe(paragraphStart);
      } else {
        expect(editor.state.selection.empty).toBe(false);
        expect(editor.state.selection.from).toBe(0);
        expect(editor.state.selection.to).toBe(editor.state.doc.content.size);
      }
    } finally {
      platformGetter.mockRestore();
    }
  },
);

async function settle() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 140));
  });
}
function key(host: HTMLElement, key: string, extra: KeyboardEventInit = {}) {
  const event = new KeyboardEvent("keydown", {
    key,
    bubbles: true,
    cancelable: true,
    ...extra,
  });
  act(() => host.querySelector(".composer-editor")!.dispatchEvent(event));
  return event;
}

test("Two Tabs open and then insert a folder; write survives submission", async () => {
  const view = fixture();
  type(view.host, "Please check ./s");
  await settle();
  expect(discoverPaths).toHaveBeenLastCalledWith(
    `/sessions/${view.id}`,
    "./s",
    expect.any(AbortSignal),
  );
  expect(view.host.querySelector(".composer-completion-disclosure")).toBeNull();
  await act(async () => {
    expect(key(view.host, "Tab").defaultPrevented).toBe(true);
  });
  expect(view.host.querySelector(".reference-completion-heading")).toBeNull();
  expect(view.store.snapshot().text).toBe("Please check ./src/");
  expect(view.store.snapshot().document).toBeNull();
  expect(view.host.querySelector('[aria-selected="true"]')?.textContent).toBe(
    "/project/src",
  );
  expect(key(view.host, "Tab", { repeat: true }).defaultPrevented).toBe(true);
  expect(view.store.snapshot().document).toBeNull();
  await act(async () => key(view.host, "Tab"));
  expect(view.send).not.toHaveBeenCalled();
  const doc = view.store.snapshot().document!;
  expect(doc.content).toHaveLength(2);
  expect(Object.values(doc.references)[0]).toEqual({
    label: "src",
    target: {
      type: "filesystem",
      original: "./src",
      path: "/project/src",
      target: "directory",
      access: { read: true, write: false },
    },
  });
  act(() =>
    view.host
      .querySelector<HTMLButtonElement>(
        '[aria-label="Allow write access to src"]',
      )!
      .click(),
  );
  await submitForm(view.host);
  expect(view.send).toHaveBeenCalledOnce();
  expect(view.send.mock.calls[0]?.[2]).toMatchObject({
    references: expect.objectContaining({
      [Object.keys(doc.references)[0]]: expect.objectContaining({
        target: expect.objectContaining({
          access: { read: true, write: true },
        }),
      }),
    }),
  });
});

test("A typed slash offers the folder first, while a child prefix selects matching children", async () => {
  const view = fixture();
  vi.mocked(discoverPaths).mockImplementation(async (_base, prefix) => {
    const result = paths(prefix, "notes.md", "file");
    return {
      ...result,
      directory: "/project/src",
      entries: result.entries.map((entry) => ({
        ...entry,
        path: "/project/src/notes.md",
      })),
    };
  });
  type(view.host, "./src/");
  await settle();
  const selected = () =>
    view.host.querySelector('[aria-selected="true"]')?.textContent;
  expect(selected()).toBe("/project/src");
  key(view.host, "ArrowDown");
  expect(selected()).toBe("notes.md");
  act(() => {
    const editor = editorOf(view.host) as unknown as Editor;
    editor.commands.insertContent("n");
  });
  await settle();
  expect(selected()).toBe("notes.md");
  expect(
    view.host.querySelector('[role="listbox"]')?.textContent,
  ).not.toContain("/project/src");
  await act(async () => key(view.host, "Enter"));
  expect(
    Object.values(view.store.snapshot().document!.references)[0]?.target,
  ).toMatchObject({
    path: "/project/src/notes.md",
    original: "./src/notes.md",
    target: "file",
  });
  expect(key(view.host, "Enter", { repeat: true }).defaultPrevented).toBe(true);
  expect(view.send).not.toHaveBeenCalled();
});

test("An empty directory still has an insertion action", async () => {
  const view = fixture();
  vi.mocked(discoverPaths).mockResolvedValue({
    ...paths("./empty/"),
    directory: "/project/empty",
    entries: [],
  });
  type(view.host, "./empty/");
  await settle();
  expect(view.host.querySelector('[aria-selected="true"]')?.textContent).toBe(
    "/project/empty",
  );
  await act(async () => key(view.host, "Tab"));
  expect(
    Object.values(view.store.snapshot().document!.references)[0]?.target,
  ).toMatchObject({
    path: "/project/empty",
    target: "directory",
  });
});

test("click accepts an exact agent and does not send or notify", async () => {
  const view = fixture();
  type(view.host, "Ask @ex");
  await settle();
  expect(view.host.querySelector("[role=listbox]")?.textContent).toContain(
    "@exact-agent",
  );
  await act(async () => {
    view.host.querySelector<HTMLButtonElement>('[role="option"]')!.click();
  });
  expect(Object.values(view.store.snapshot().document!.references)[0]).toEqual({
    label: "exact-agent",
    target: { type: "agent", session: "exact-agent" },
  });
  expect(view.send).not.toHaveBeenCalled();
  expect(discoverAgents).toHaveBeenCalledOnce();
});

test("loading, empty and failed lookups consume Enter; Escape dismisses without changing the draft", async () => {
  const view = fixture();
  vi.mocked(discoverPaths).mockResolvedValue({
    ...paths("./"),
    entries: [],
    problem: "missing",
  });
  type(view.host, "./");
  key(view.host, "Enter");
  expect(view.send).not.toHaveBeenCalled();
  await settle();
  expect(view.host.querySelector('[role="status"]')?.textContent).toContain(
    "Could not list",
  );
  key(view.host, "Enter");
  expect(view.send).not.toHaveBeenCalled();
  key(view.host, "Escape");
  expect(view.host.querySelector("[role=listbox]")).toBeNull();
  expect(view.store.snapshot().text).toBe("./");
  await act(async () => editorOf(view.host).chain().focus("end").run());
  expect(view.host.querySelector("[role=listbox]")).toBeNull();
});

test("composition confirmation cannot accept a reference or send", async () => {
  const view = fixture();
  type(view.host, "./");
  await settle();
  key(view.host, "Enter", { isComposing: true });
  expect(view.send).not.toHaveBeenCalled();
  expect(view.store.snapshot().document).toBeNull();
});

test("late responses cannot overwrite a new prefix, an Escape dismissal or another session", async () => {
  const view = fixture();
  let resolve!: (result: PathCandidates) => void;
  vi.mocked(discoverPaths).mockImplementationOnce(
    () =>
      new Promise((done) => {
        resolve = done;
      }),
  );
  type(view.host, "./old");
  await settle();
  type(view.host, "./new");
  await settle();
  await act(async () => resolve(paths("./old", "obsolete")));
  expect(view.host.querySelector("[role=listbox]")?.textContent).not.toContain(
    "obsolete",
  );
  key(view.host, "Escape");
  expect(view.host.querySelector("[role=listbox]")).toBeNull();
  vi.mocked(discoverPaths).mockImplementationOnce(
    () =>
      new Promise((done) => {
        resolve = done;
      }),
  );
  type(view.host, "./late");
  await settle();
  view.render(`other-${crypto.randomUUID()}`);
  await act(async () => resolve(paths("./late", "leaked")));
  expect(view.host.querySelector("[role=listbox]")).toBeNull();
  expect(view.host.querySelector(".composer-editor")?.textContent).toBe("");
});

test.each(["paragraph", "hardBreak"])(
  "completion accepts on later lines after %s",
  async (separator) => {
    const view = fixture();
    type(view.host, "First line");
    act(() => {
      const editor = editorOf(view.host) as unknown as Editor;
      if (separator === "paragraph") editor.commands.splitBlock();
      else editor.commands.setHardBreak();
      editor.commands.insertContent("./s");
    });
    await settle();
    expect(view.host.querySelectorAll('[role="option"]')).toHaveLength(1);
    await act(async () => key(view.host, "Tab"));
    await act(async () => key(view.host, "Tab"));
    expect(view.store.snapshot().document?.content).toEqual([
      { Text: "First line\n" },
      { Reference: expect.any(String) },
    ]);
    expect(view.send).not.toHaveBeenCalled();
  },
);

test("session suggestions exclude self and insert the selected session reference", async () => {
  const view = fixture();
  vi.mocked(discoverAgents).mockImplementation(async (_base, prefix) => ({
    prefix,
    truncated: false,
    agents: [
      { id: view.id, status: "idle", processes: 0 },
      { id: "another-agent", status: "idle", processes: 0 },
    ],
  }));
  type(view.host, "@");
  await settle();
  const options = view.host.querySelectorAll('[role="option"]');
  expect(options).toHaveLength(1);
  expect(options[0]?.textContent).toContain("@another-agent");
  expect(options[0]?.getAttribute("aria-selected")).toBe("true");
  await act(async () => key(view.host, "Tab"));
  expect(
    Object.values(view.store.snapshot().document!.references)[0]?.target,
  ).toEqual({
    type: "agent",
    session: "another-agent",
  });
  expect(view.send).not.toHaveBeenCalled();

  vi.mocked(discoverAgents).mockImplementation(async (_base, prefix) => ({
    prefix,
    truncated: false,
    agents: [{ id: view.id, status: "idle", processes: 0 }],
  }));
  type(view.host, `@${view.id}`);
  await settle();
  expect(view.host.querySelectorAll('[role="option"]')).toHaveLength(0);
  expect(view.host.querySelector('[role="status"]')?.textContent).toContain(
    "No other sessions match",
  );
  key(view.host, "Enter");
  expect(view.send).not.toHaveBeenCalled();
});
