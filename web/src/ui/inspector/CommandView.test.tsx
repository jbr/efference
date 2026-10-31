// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, test, vi } from "vitest";
import { CommandView } from "./CommandView.tsx";
import { fetchProcessRecord } from "../../api/http.ts";
vi.mock("../../api/http.ts", () => ({
  fetchProcessRecord: vi.fn(),
  killProcess: vi.fn(),
  patchProcess: vi.fn(),
}));
vi.mock("../Code.tsx", () => ({
  Code: ({ text }: { text: string }) => <pre>{text}</pre>,
}));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
afterEach(() => {
  vi.resetAllMocks();
  vi.unstubAllGlobals();
});
test("running process becomes finished with final output and no follow/action controls", async () => {
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  const process = {
    id: 7,
    call_id: null,
    pid: 123,
    command: "demo",
    spawned_at: Date.now(),
    background: true,
    muted: false,
  };
  vi.mocked(fetchProcessRecord).mockResolvedValue({
    ...process,
    running: false,
    exit: { outcome: { code: 7 }, duration_ms: 60000, at: Date.now() },
  });
  const host = document.createElement("div");
  const root = createRoot(host);
  try {
    await act(async () =>
      root.render(
        <CommandView
          process={process}
          processId={7}
          command="demo"
          output="tick"
          lost={0}
        />,
      ),
    );
    expect(host.textContent).toContain("Running");
    expect(host.textContent).toContain("Following");
    expect(host.textContent).toContain("Kill");
    await act(async () =>
      root.render(
        <CommandView
          processId={7}
          command="demo"
          output={"FINAL STDOUT\nFINAL STDERR"}
          lost={0}
        />,
      ),
    );
    expect(host.textContent).toContain("Finished · exit 7 · 60.0s");
    expect(host.querySelector(".command-output")?.textContent).toBe(
      "FINAL STDOUT\nFINAL STDERR",
    );
    expect(host.textContent).not.toContain("Following");
    expect(host.textContent).not.toContain("Kill");
  } finally {
    act(() => root.unmount());
  }
});
