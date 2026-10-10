// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, test, vi } from "vitest";
import { ComposerToolbar } from "./ComposerToolbar.tsx";
import { turnState, turnClass } from "./turnState.ts";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

test("phone waiting readout keeps the full condition and omits desktop hints", () => {
  const host = document.createElement("form");
  const root = createRoot(host);
  const attach = vi.fn();
  const interrupt = vi.fn();
  const resume = vi.fn();
  act(() =>
    root.render(
      <ComposerToolbar
        turn={turnState(true, null, false, false, null, null, "ask")}
        narrow
        running
        held
        onAttach={attach}
        onInterrupt={interrupt}
        onResume={resume}
      />,
    ),
  );
  expect(host.querySelector(".turn-state")?.textContent).toBe(
    "waiting for your answer",
  );
  expect(host.querySelector(".turn-state")?.className).toBe(
    "turn-state waiting",
  );
  expect(host.querySelector(".send-kbd")).toBeNull();
  expect(host.querySelector(".composer-hint")).toBeNull();
  act(() => {
    host.querySelector<HTMLButtonElement>(".attach-btn")?.click();
    host.querySelector<HTMLButtonElement>(".interrupt-btn")?.click();
    host.querySelector<HTMLButtonElement>(".resume-btn")?.click();
  });
  expect(attach).toHaveBeenCalledOnce();
  expect(interrupt).toHaveBeenCalledOnce();
  expect(resume).toHaveBeenCalledOnce();
  expect(host.querySelector<HTMLButtonElement>(".send-btn")?.type).toBe(
    "submit",
  );
  act(() => root.unmount());
});

test("desktop idle controls retain send hint and disable send while uploading", () => {
  const host = document.createElement("form");
  const root = createRoot(host);
  act(() =>
    root.render(
      <ComposerToolbar
        turn={turnState(false, null)}
        narrow={false}
        running={false}
        uploading
        onAttach={() => {}}
        onInterrupt={() => {}}
        onResume={() => {}}
      />,
    ),
  );
  expect(host.textContent).toContain("your turn");
  expect(host.textContent).toContain("shift-enter for newline");
  expect(host.querySelector(".send-kbd")?.textContent).toBe("↵");
  expect(host.querySelector(".interrupt-btn")).toBeNull();
  expect(host.querySelector<HTMLButtonElement>(".send-btn")?.disabled).toBe(
    true,
  );
  act(() => root.unmount());
});

test("shared readout classes preserve each condition", () => {
  expect(
    ["running", "idle", "stalled", "parked", "waiting"].map((kind) =>
      turnClass(kind as Parameters<typeof turnClass>[0]),
    ),
  ).toEqual([
    "turn-state dim",
    "turn-state your-turn",
    "turn-state stalled",
    "turn-state parked",
    "turn-state waiting",
  ]);
});
