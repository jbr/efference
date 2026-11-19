// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, test } from "vitest";
import { Interface } from "./Interface.tsx";
import { setDevSetting } from "../session/devSettings.ts";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

test("the dev tooling switches flip their settings from the panel", () => {
  const host = document.createElement("div");
  const root = createRoot(host);
  try {
    act(() => root.render(<Interface />));
    const input = (label: string) =>
      [...host.querySelectorAll("label")]
        .find((row) => row.textContent?.includes(label))
        ?.querySelector("input") as HTMLInputElement;
    const scrub = input("stream scrubber");
    const debug = input("stream debug");
    expect(scrub.checked).toBe(false);
    expect(debug.checked).toBe(false);
    act(() => scrub.click());
    expect(scrub.checked).toBe(true);
    expect(debug.checked).toBe(false);
    act(() => debug.click());
    expect(debug.checked).toBe(true);
  } finally {
    act(() => root.unmount());
    setDevSetting("scrub", false);
    setDevSetting("streamDebug", false);
  }
});
