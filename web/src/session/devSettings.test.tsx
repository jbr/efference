// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, test, vi } from "vitest";
import {
  setDevSetting,
  useDevSettings,
  type DevSettings,
} from "./devSettings.ts";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

test("dev tooling is off until switched on, and the switch persists", () => {
  let seen: DevSettings | null = null;
  function Probe() {
    seen = useDevSettings();
    return null;
  }
  const root = createRoot(document.createElement("div"));
  act(() => root.render(<Probe />));
  try {
    expect(seen).toEqual({ scrub: false, streamDebug: false });
    act(() => setDevSetting("scrub", true));
    expect(seen).toEqual({ scrub: true, streamDebug: false });
    expect(JSON.parse(localStorage.getItem("dev")!)).toEqual({
      scrub: true,
      streamDebug: false,
    });
  } finally {
    act(() => root.unmount());
    localStorage.clear();
  }
});

test("a stored switch comes back on a reload", async () => {
  localStorage.setItem("dev", JSON.stringify({ streamDebug: true }));
  vi.resetModules();
  const fresh = await import("./devSettings.ts");
  let seen: DevSettings | null = null;
  function Probe() {
    seen = fresh.useDevSettings();
    return null;
  }
  const root = createRoot(document.createElement("div"));
  act(() => root.render(<Probe />));
  try {
    expect(seen).toEqual({ scrub: false, streamDebug: true });
  } finally {
    act(() => root.unmount());
    localStorage.clear();
  }
});
