// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, test, vi } from "vitest";
import { FileOperation } from "./FileOperation.tsx";
vi.mock("../highlight.ts", () => ({ useHighlight: () => null }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
test("captured reads start with retained lines and open current file only on request", () => {
  const host = document.createElement("div");
  const root = createRoot(host);
  const onCurrent = vi.fn();
  try {
    act(() =>
      root.render(
        <FileOperation
          name="read"
          input={{ paths: ["a.rs"] }}
          raw={{
            content:
              "==t BEGIN /root/a.rs, LENGTH: 4 t==\none\ntwo\nthree\n==t END /root/a.rs t==",
            is_error: false,
          }}
          wire={{
            Distilled: { keep: { Ranges: [[3, 3]] }, note: "selected middle" },
          }}
          onCurrent={onCurrent}
        />,
      ),
    );
    expect(host.querySelectorAll("[data-line]")).toHaveLength(1);
    expect(host.querySelector("[data-line]")?.textContent).toContain("two");
    expect(onCurrent).not.toHaveBeenCalled();
    const open = [...host.querySelectorAll("button")].find(
      (button) => button.textContent === "View current file →",
    )!;
    act(() => open.click());
    expect(onCurrent).toHaveBeenCalledWith("a.rs");
  } finally {
    act(() => root.unmount());
  }
});
test("append shows submitted content, explicitly not a historical whole file", () => {
  const host = document.createElement("div");
  const root = createRoot(host);
  try {
    act(() =>
      root.render(
        <FileOperation
          name="write"
          input={{ path: "a.rs", contents: "NEW BYTES", append: true }}
          raw={{ content: "written", is_error: false }}
          wire="Raw"
          onCurrent={() => {}}
        />,
      ),
    );
    expect(host.textContent).toContain("Appended content");
    expect(host.textContent).toContain("not the whole file");
    expect(host.querySelector("pre")?.textContent).toContain("NEW BYTES");
  } finally {
    act(() => root.unmount());
  }
});

test("edit inspection shows recorded removal and addition, not just triaged rows", () => {
  const host = document.createElement("div");
  const root = createRoot(host);
  const onCurrent = vi.fn();
  try {
    act(() =>
      root.render(
        <FileOperation
          name="edit"
          input={{ file_path: "a.rs" }}
          raw={{ content: "edited\n===DIFF===\n-old\n+new", is_error: false }}
          wire={{
            Distilled: { keep: { Ranges: [[4, 4]] }, note: "kept addition" },
          }}
          onCurrent={onCurrent}
        />,
      ),
    );
    expect(host.textContent).toContain("Recorded edit");
    expect(host.textContent).toContain("-old");
    expect(host.textContent).toContain("+new");
    act(() =>
      [...host.querySelectorAll("button")]
        .find((button) => button.textContent === "Locate in current file →")!
        .click(),
    );
    expect(onCurrent).toHaveBeenCalledWith("a.rs");
  } finally {
    act(() => root.unmount());
  }
});
