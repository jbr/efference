// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, test, vi } from "vitest";
import { FileView } from "./FileView.tsx";
import {
  fetchFile,
  fileStream,
  type ChangeStream,
  type ChangeStreamCallbacks,
} from "../api/files.ts";
vi.mock("../api/files.ts", () => ({ fetchFile: vi.fn(), fileStream: vi.fn() }));
vi.mock("../session/useFileTree.ts", () => ({ useFileTree: () => ({}) }));
vi.mock("./FileTree.tsx", () => ({ FileTreePane: () => null }));
vi.mock("./Markdown.tsx", () => ({
  Markdown: ({ children }: { children: string }) => <pre>{children}</pre>,
}));
vi.mock("./Code.tsx", () => ({
  NumberedCode: ({
    lines,
    lineClass,
  }: {
    lines: [number, string][];
    lineClass?: (n: number) => string | undefined;
  }) => (
    <pre>
      {lines.map(([n, text]) => (
        <div key={n} data-line={n} className={lineClass?.(n)}>
          {text}
        </div>
      ))}
    </pre>
  ),
}));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
afterEach(() => {
  vi.resetAllMocks();
  vi.unstubAllGlobals();
  // jsdom's `hidden` is a getter on the prototype; drop a test's override.
  Reflect.deleteProperty(document, "hidden");
});

/** Stand in for the change stream: each `fileStream` call makes a stream the
 * test can drive through the callbacks the view handed over. */
function streams() {
  const opened: {
    path: string;
    callbacks: ChangeStreamCallbacks;
    stream: ChangeStream;
  }[] = [];
  vi.mocked(fileStream).mockImplementation((path, callbacks) => {
    const stream = { close: vi.fn() };
    opened.push({ path, callbacks, stream });
    return stream;
  });
  return opened;
}

/** Whether the tab is visible, as `document.hidden` reports it. */
function setHidden(hidden: boolean) {
  Object.defineProperty(document, "hidden", {
    configurable: true,
    get: () => hidden,
  });
  document.dispatchEvent(new Event("visibilitychange"));
}

const viewProps = {
  root: "project",
  path: "a.rs",
  openKey: 0,
  openAnchor: null,
  lastEdit: null,
  activity: new Map(),
  closing: false,
  onSlideOut: () => {},
  onClose: () => {},
};

test("historical file target stays put through a later edit and keeps selected anchor through refresh", async () => {
  vi.stubGlobal("localStorage", { getItem: () => null, setItem: () => {} });
  const opened = streams();
  vi.mocked(fetchFile).mockResolvedValue({
    kind: "ok",
    text: "old\nselected\ntail",
    etag: "1",
  });
  const host = document.createElement("div");
  const root = createRoot(host);
  const base = {
    root: "project",
    path: "a.rs",
    openKey: 0,
    openAnchor: { added: ["selected"], before: ["old"], after: ["tail"] },
    activity: new Map(),
    closing: false,
    onSlideOut: () => {},
    onClose: () => {},
  };
  try {
    await act(async () => root.render(<FileView {...base} lastEdit={null} />));
    const shownPath = () =>
      host.querySelector('.file-path [aria-current="page"]')?.textContent;
    expect(shownPath()).toBe("a.rs");
    expect(host.querySelector(".changed")?.textContent).toBe("selected");
    vi.mocked(fetchFile).mockResolvedValue({
      kind: "ok",
      text: "prefix\nold\nselected\ntail",
      etag: "2",
    });
    await act(async () => opened[0].callbacks.onChange("a.rs"));
    expect(host.querySelector(".changed")?.getAttribute("data-line")).toBe("3");
    const latest = { seq: 10, path: "b.rs", anchor: null };
    await act(async () =>
      root.render(<FileView {...base} lastEdit={latest} />),
    );
    expect(shownPath()).toBe("a.rs");
  } finally {
    act(() => root.unmount());
  }
});
test("network failure is visible rather than an unhandled rejection", async () => {
  vi.stubGlobal("localStorage", { getItem: () => null, setItem: () => {} });
  streams();
  vi.mocked(fetchFile).mockRejectedValue(new Error("offline"));
  const host = document.createElement("div");
  const root = createRoot(host);
  try {
    await act(async () => root.render(<FileView {...viewProps} />));
    expect(host.textContent).toContain("offline");
  } finally {
    act(() => root.unmount());
  }
});

test("the stream's open and each poke are answered with the conditional GET", async () => {
  vi.stubGlobal("localStorage", { getItem: () => null, setItem: () => {} });
  const opened = streams();
  vi.mocked(fetchFile).mockResolvedValue({
    kind: "ok",
    text: "one",
    etag: "1",
  });
  const host = document.createElement("div");
  const root = createRoot(host);
  try {
    await act(async () => root.render(<FileView {...viewProps} />));
    expect(opened.map((o) => o.path)).toEqual(["a.rs"]);
    // The view's own load is unconditional; it carries no validator yet.
    expect(vi.mocked(fetchFile).mock.calls).toEqual([["a.rs", null]]);

    await act(async () => opened[0].callbacks.onOpen());
    await act(async () => opened[0].callbacks.onChange("a.rs"));
    expect(vi.mocked(fetchFile).mock.calls).toEqual([
      ["a.rs", null],
      ["a.rs", "1"],
      ["a.rs", "1"],
    ]);
  } finally {
    act(() => root.unmount());
  }
});

test("the stream is held while the tab is visible and closed while it is hidden", async () => {
  vi.stubGlobal("localStorage", { getItem: () => null, setItem: () => {} });
  const opened = streams();
  vi.mocked(fetchFile).mockResolvedValue({
    kind: "ok",
    text: "one",
    etag: "1",
  });
  const host = document.createElement("div");
  const root = createRoot(host);
  try {
    await act(async () => root.render(<FileView {...viewProps} />));
    expect(opened).toHaveLength(1);

    act(() => setHidden(true));
    expect(opened[0].stream.close).toHaveBeenCalledTimes(1);
    expect(opened).toHaveLength(1);

    // Visible again: a fresh stream, whose open covers what the hidden
    // interval missed.
    act(() => setHidden(false));
    expect(opened).toHaveLength(2);
    expect(opened[1].stream.close).not.toHaveBeenCalled();
    await act(async () => opened[1].callbacks.onOpen());
    expect(vi.mocked(fetchFile)).toHaveBeenLastCalledWith("a.rs", "1");
  } finally {
    act(() => root.unmount());
  }
  // Leaving closes whichever stream is held.
  expect(opened[1].stream.close).toHaveBeenCalledTimes(1);
});

test("a view opened in a hidden tab streams nothing until it is shown", async () => {
  vi.stubGlobal("localStorage", { getItem: () => null, setItem: () => {} });
  const opened = streams();
  vi.mocked(fetchFile).mockResolvedValue({
    kind: "ok",
    text: "one",
    etag: "1",
  });
  Object.defineProperty(document, "hidden", {
    configurable: true,
    get: () => true,
  });
  const host = document.createElement("div");
  const root = createRoot(host);
  try {
    await act(async () => root.render(<FileView {...viewProps} />));
    expect(opened).toHaveLength(0);
    act(() => setHidden(false));
    expect(opened).toHaveLength(1);
  } finally {
    act(() => root.unmount());
  }
});
