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
        turn={turnState({ phase: "asking" })}
        narrow
        running
        held
        onAttach={attach}
        onInterrupt={interrupt}
        onHoldChange={resume}
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
    host.querySelector<HTMLButtonElement>(".hold-btn")?.click();
  });
  expect(attach).toHaveBeenCalledOnce();
  expect(interrupt).toHaveBeenCalledOnce();
  expect(resume).toHaveBeenCalledExactlyOnceWith(false);
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
        turn={turnState({ phase: "idle" })}
        narrow={false}
        running={false}
        uploading
        onAttach={() => {}}
        onInterrupt={() => {}}
        onHoldChange={() => {}}
      />,
    ),
  );
  expect(host.textContent).toContain("your turn");
  expect(host.textContent).toContain("shift-enter for newline");
  expect(host.querySelector(".send-kbd")).toBeNull();
  expect(host.querySelector(".send-btn")?.getAttribute("aria-label")).toBe(
    "Send message",
  );
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

test("hold requests pause after the step and pending changes cannot be repeated", () => {
  const host = document.createElement("form");
  const root = createRoot(host);
  const hold = vi.fn();
  const props = {
    turn: turnState({ phase: "idle" }),
    narrow: true,
    running: false,
    onAttach: () => {},
    onInterrupt: () => {},
    onHoldChange: hold,
  };
  try {
    act(() => root.render(<ComposerToolbar {...props} />));
    const button = host.querySelector<HTMLButtonElement>(".hold-btn")!;
    expect(button.type).toBe("button");
    expect(button.getAttribute("aria-label")).toBe(
      "Hold after the current step",
    );
    act(() => button.click());
    expect(hold).toHaveBeenCalledExactlyOnceWith(true);
    act(() => root.render(<ComposerToolbar {...props} holdPending />));
    expect(button.disabled).toBe(true);
    act(() => button.click());
    expect(hold).toHaveBeenCalledOnce();
    act(() => root.render(<ComposerToolbar {...props} held />));
    expect(button.getAttribute("aria-pressed")).toBe("true");
    expect(button.getAttribute("aria-label")).toBe("Release hold");
    expect(
      [...host.querySelectorAll("button")].filter((b) => b.type === "submit"),
    ).toHaveLength(1);
  } finally {
    act(() => root.unmount());
  }
});
