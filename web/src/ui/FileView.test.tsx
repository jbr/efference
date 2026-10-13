// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, test, vi } from "vitest";
import { FileView } from "./FileView.tsx";
import { fetchFile, fileStream } from "../api/files.ts";
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
});
test("historical file target stays put until explicit follow and keeps selected anchor through refresh", async () => {
  vi.stubGlobal("localStorage", { getItem: () => null, setItem: () => {} });
  const source = Object.assign(new EventTarget(), {
    close: vi.fn(),
    onopen: null,
  });
  vi.mocked(fileStream).mockReturnValue(source as unknown as EventSource);
  vi.mocked(fetchFile).mockResolvedValue({
    kind: "ok",
    text: "old\nselected\ntail",
    etag: "1",
  });
  const host = document.createElement("div");
  const root = createRoot(host);
  const base = {
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
    expect(host.textContent).toContain("current file");
    expect(host.textContent).toContain("follow");
    expect(host.textContent).not.toContain("following ⇢");
    expect(host.querySelector(".changed")?.textContent).toBe("selected");
    vi.mocked(fetchFile).mockResolvedValue({
      kind: "ok",
      text: "prefix\nold\nselected\ntail",
      etag: "2",
    });
    await act(async () => source.dispatchEvent(new Event("change")));
    expect(host.querySelector(".changed")?.getAttribute("data-line")).toBe("3");
    const latest = { seq: 10, path: "b.rs", anchor: null };
    await act(async () =>
      root.render(<FileView {...base} lastEdit={latest} />),
    );
    expect(host.querySelector(".file-view-path")?.textContent).toBe("a.rs");
    act(() =>
      [...host.querySelectorAll("button")]
        .find((button) => button.textContent === "follow")!
        .click(),
    );
    await act(async () =>
      root.render(<FileView {...base} lastEdit={{ ...latest, seq: 11 }} />),
    );
    expect(host.querySelector(".file-view-path")?.textContent).toBe("b.rs");
  } finally {
    act(() => root.unmount());
  }
});
test("network failure is visible rather than an unhandled rejection", async () => {
  vi.stubGlobal("localStorage", { getItem: () => null, setItem: () => {} });
  vi.mocked(fileStream).mockReturnValue(
    Object.assign(new EventTarget(), {
      close: () => {},
      onopen: null,
    }) as unknown as EventSource,
  );
  vi.mocked(fetchFile).mockRejectedValue(new Error("offline"));
  const host = document.createElement("div");
  const root = createRoot(host);
  try {
    await act(async () =>
      root.render(
        <FileView
          path="a.rs"
          openKey={0}
          openAnchor={null}
          lastEdit={null}
          activity={new Map()}
          closing={false}
          onSlideOut={() => {}}
          onClose={() => {}}
        />,
      ),
    );
    expect(host.textContent).toContain("offline");
  } finally {
    act(() => root.unmount());
  }
});
