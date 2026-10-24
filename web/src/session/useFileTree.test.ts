// @vitest-environment jsdom
import { act, createElement, useEffect } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  dirStream,
  listDir,
  type ChangeStream,
  type ChangeStreamCallbacks,
} from "../api/files.ts";
import { ancestors, useFileTree, type FileTree } from "./useFileTree.ts";

vi.mock("../api/files.ts", () => ({ dirStream: vi.fn(), listDir: vi.fn() }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
afterEach(() => vi.resetAllMocks());

describe("ancestors", () => {
  it("names every directory to open to reveal a path, root first", () => {
    expect(ancestors("web/src/ui/FileView.tsx")).toEqual([
      "",
      "web",
      "web/src",
      "web/src/ui",
    ]);
  });

  it("is the root alone for a file at the top", () => {
    expect(ancestors("README.md")).toEqual([""]);
  });
});

describe("the tree's change streams", () => {
  /** Each `dirStream` call, with the callbacks the tree handed over. */
  function streams() {
    const opened: {
      path: string;
      callbacks: ChangeStreamCallbacks;
      stream: ChangeStream & { close: ReturnType<typeof vi.fn> };
    }[] = [];
    vi.mocked(dirStream).mockImplementation((path, callbacks) => {
      const stream = { close: vi.fn() };
      opened.push({ path, callbacks, stream });
      return stream;
    });
    vi.mocked(listDir).mockResolvedValue({
      kind: "ok",
      listing: { path: "", recursive: false, truncated: false, entries: [] },
    });
    return opened;
  }

  function mountTree() {
    let latest!: FileTree;
    function Harness() {
      const tree = useFileTree();
      useEffect(() => {
        latest = tree;
      });
      return null;
    }
    const root = createRoot(document.createElement("div"));
    act(() => root.render(createElement(Harness)));
    return { tree: () => latest, unmount: () => act(() => root.unmount()) };
  }

  it("holds a stream for the open root, and answers its open and each poke with a re-list", async () => {
    const opened = streams();
    const view = mountTree();
    expect(opened.map((o) => o.path)).toEqual([""]);
    expect(listDir).not.toHaveBeenCalled();

    await act(async () => opened[0].callbacks.onOpen());
    await act(async () => opened[0].callbacks.onChange(""));
    expect(vi.mocked(listDir).mock.calls).toEqual([[""], [""]]);
    expect(view.tree().dirs.get("")?.loading).toBe(false);
    view.unmount();
  });

  it("opens a stream for each directory expanded, and closes it when collapsed", async () => {
    const opened = streams();
    const view = mountTree();
    await act(async () => view.tree().toggle("src"));
    expect(opened.map((o) => o.path)).toEqual(["", "src"]);

    await act(async () => opened[1].callbacks.onChange("src"));
    expect(vi.mocked(listDir).mock.calls).toEqual([["src"]]);

    await act(async () => view.tree().toggle("src"));
    expect(opened[1].stream.close).toHaveBeenCalledTimes(1);
    expect(opened[0].stream.close).not.toHaveBeenCalled();
    view.unmount();
  });

  it("closes every stream it holds when the tree leaves", async () => {
    const opened = streams();
    const view = mountTree();
    await act(async () => view.tree().reveal("a/b/c"));
    expect(opened.map((o) => o.path)).toEqual(["", "a", "a/b"]);
    view.unmount();
    for (const { stream } of opened) {
      expect(stream.close).toHaveBeenCalledTimes(1);
    }
  });
});
