// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, test, vi } from "vitest";
import { Composer } from "./Composer.tsx";
import { acquireDraft } from "./drafts.ts";
import type { ComposerStatusMessage } from "./ComposerStatus.tsx";

const viewport = vi.hoisted(() => ({ narrow: false }));
vi.mock("./useNarrow.ts", () => ({ useNarrow: () => viewport.narrow }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
afterEach(() => vi.unstubAllGlobals());

for (const narrow of [false, true]) {
  test(`exclusive composer error and recovery in ${narrow ? "collapsed phone" : "expanded desktop"} footer`, () => {
    viewport.narrow = narrow;
    const id = `status-${crypto.randomUUID()}`;
    history.replaceState(null, "", `/sessions/${id}/`);
    const store = acquireDraft(`/sessions/${id}`);
    store.setText("preserve my draft");
    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    const onRetry = vi.fn();
    const props = {
      submit: vi.fn(() => Promise.resolve()),
      recalled: null,
      pace: null,
      phase: { phase: "requesting" } as const,
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
    const render = (status: ComposerStatusMessage | null) =>
      act(() => root.render(<Composer {...props} status={status} />));
    const footer = () =>
      host.querySelector(narrow ? ".status-bar" : ".composer-toolbar")!;
    try {
      render({
        text: "Could not start",
        error: true,
        detail: "MCP timed out",
        action: { label: "Retry", pending: false, onClick: onRetry },
      });
      expect(footer().querySelector(".turn-state")).toBeNull();
      expect(footer().querySelector("[role=alert]")?.textContent).toContain(
        "MCP timed out",
      );
      expect(footer().querySelector(".interrupt-btn")).not.toBeNull();
      act(() =>
        footer()
          .querySelector<HTMLButtonElement>(".composer-status button")!
          .click(),
      );
      expect(onRetry).toHaveBeenCalledOnce();
      render({
        text: "retrying startup…",
        error: true,
        detail: "MCP timed out",
        action: { label: "Retrying…", pending: true, onClick: onRetry },
      });
      expect(
        footer().querySelector<HTMLButtonElement>(".composer-status button")!
          .disabled,
      ).toBe(true);
      expect(store.snapshot().text).toBe("preserve my draft");
      expect(host.querySelector("textarea")?.value).toBe("preserve my draft");
      render({ text: "reconnecting…" });
      expect(footer().querySelector("[role=status]")?.textContent).toBe(
        "reconnecting…",
      );
      expect(footer().querySelector(".turn-state")).toBeNull();
      render(null);
      expect(footer().querySelector(".composer-status")).toBeNull();
      expect(footer().querySelector(".turn-state")).not.toBeNull();
    } finally {
      act(() => root.unmount());
      host.remove();
    }
  });
}
