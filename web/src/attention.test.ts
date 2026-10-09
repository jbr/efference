import { describe, expect, it } from "vitest";
import {
  Attender,
  HEARTBEAT_MS,
  INPUT_THROTTLE_MS,
  INPUT_WINDOW_MS,
  isActive,
} from "./attention.ts";

const at = (over: Partial<Parameters<typeof isActive>[0]> = {}) =>
  isActive({
    visible: true,
    focused: true,
    lastInputAt: 0,
    now: 1_000,
    ...over,
  });

describe("isActive", () => {
  it("needs visible, focused, and recent input", () => {
    expect(at()).toBe(true);
    expect(at({ visible: false })).toBe(false);
    expect(at({ focused: false })).toBe(false);
    expect(at({ lastInputAt: null })).toBe(false);
    expect(at({ now: INPUT_WINDOW_MS - 1 })).toBe(true);
    expect(at({ now: INPUT_WINDOW_MS })).toBe(false);
  });
});

function attending(viewing: string | null = "s1") {
  const a = new Attender("c1");
  a.visible = true;
  a.focused = true;
  a.viewing = viewing;
  return a;
}

describe("Attender", () => {
  it("says nothing before there is anything to say", () => {
    const a = new Attender("c1");
    expect(a.due(0)).toBeNull();
  });

  it("reports activation at once, then only a heartbeat", () => {
    const a = attending();
    a.input(0);
    expect(a.due(0)).toEqual({ client: "c1", viewing: "s1", active: true });
    expect(a.due(1_000)).toBeNull();
    a.input(HEARTBEAT_MS - 1);
    expect(a.due(HEARTBEAT_MS - 1)).toBeNull();
    expect(a.due(HEARTBEAT_MS)?.active).toBe(true);
    expect(a.due(HEARTBEAT_MS + 1)).toBeNull();
  });

  it("reports inactive as soon as the page hides or blurs", () => {
    const a = attending();
    a.input(0);
    a.due(0);
    a.visible = false;
    expect(a.due(1)).toEqual({ client: "c1", viewing: "s1", active: false });
    expect(a.due(2)).toBeNull();
    a.visible = true;
    a.focused = false;
    // Still inactive: nothing changed.
    expect(a.due(3)).toBeNull();
  });

  it("notices the input window lapsing with no event", () => {
    const a = attending();
    a.input(0);
    a.due(0);
    expect(a.due(INPUT_WINDOW_MS - 1)?.active).toBe(true); // heartbeat
    expect(a.due(INPUT_WINDOW_MS)?.active).toBe(false);
  });

  it("reports a change of session immediately, active or not", () => {
    const a = attending("s1");
    a.input(0);
    a.due(0);
    a.viewing = "s2";
    expect(a.due(1)).toEqual({ client: "c1", viewing: "s2", active: true });
    a.viewing = null;
    expect(a.due(2)).toEqual({ client: "c1", viewing: null, active: true });
  });

  it("throttles input-caused activation but not deactivation", () => {
    const a = attending();
    a.input(0);
    a.due(0);
    // The window lapses; input resumes straight away.
    const lapse = INPUT_WINDOW_MS;
    expect(a.due(lapse)?.active).toBe(false);
    a.input(lapse + 1);
    expect(a.due(lapse + 1)).toBeNull();
    expect(a.due(lapse + INPUT_THROTTLE_MS)?.active).toBe(true);
    // Deactivation by hiding is immediate.
    a.visible = false;
    expect(a.due(lapse + INPUT_THROTTLE_MS + 1)?.active).toBe(false);
  });

  it("reports a first view even when inactive", () => {
    const a = new Attender("c1");
    a.viewing = "s1";
    expect(a.due(0)).toEqual({ client: "c1", viewing: "s1", active: false });
  });
});
