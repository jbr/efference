// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { bearer } from "./api/bearer.ts";
import {
  Attender,
  type AttentionReport,
  HEARTBEAT_MS,
  INPUT_THROTTLE_MS,
  INPUT_WINDOW_MS,
  isActive,
  startAttention,
  TICK_MS,
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

function attending(session: string | null = "s1") {
  const a = new Attender("c1");
  a.visible = true;
  a.focused = true;
  a.session = session;
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
    expect(a.due(0)).toEqual({ client: "c1", session: "s1", active: true });
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
    expect(a.due(1)).toEqual({ client: "c1", session: "s1", active: false });
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
    a.session = "s2";
    expect(a.due(1)).toEqual({ client: "c1", session: "s2", active: true });
    a.session = null;
    expect(a.due(2)).toEqual({ client: "c1", session: null, active: true });
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
    a.session = "s1";
    expect(a.due(0)).toEqual({ client: "c1", session: "s1", active: false });
  });
});

describe("startAttention", () => {
  let stop: () => void;
  const send = vi.fn<(report: AttentionReport) => Promise<void>>(() =>
    Promise.resolve(),
  );

  beforeEach(() => {
    vi.useFakeTimers();
    vi.spyOn(document, "hasFocus").mockReturnValue(true);
    vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
    send.mockClear();
  });

  afterEach(async () => {
    stop();
    vi.useRealTimers();
    vi.restoreAllMocks();
    await bearer.clear();
  });

  /** A person touching the page. */
  const touch = () => window.dispatchEvent(new Event("pointerdown"));

  it("sends nothing while no bearer is held, however active the page is", () => {
    stop = startAttention(send);
    touch();
    vi.advanceTimersByTime(HEARTBEAT_MS + 2 * TICK_MS);
    touch();
    window.dispatchEvent(new Event("focus"));
    document.dispatchEvent(new Event("visibilitychange"));
    expect(send).not.toHaveBeenCalled();
  });

  it("reports as soon as a bearer is stored, and not before", async () => {
    stop = startAttention(send);
    touch();
    expect(send).not.toHaveBeenCalled();

    await bearer.store("tok");

    expect(send).toHaveBeenCalledOnce();
    expect(send).toHaveBeenCalledWith(
      expect.objectContaining({ session: null, active: true }),
    );
  });

  it("falls silent when the bearer is cleared, heartbeats included, and picks up on the next one", async () => {
    await bearer.store("tok");
    stop = startAttention(send);
    touch();
    expect(send).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(HEARTBEAT_MS);
    expect(send).toHaveBeenCalledTimes(2);

    await bearer.clear();
    touch();
    vi.advanceTimersByTime(HEARTBEAT_MS + 2 * TICK_MS);
    expect(send).toHaveBeenCalledTimes(2);

    // The heartbeat that fell due while signed out is sent as the bearer lands.
    await bearer.store("fresh");
    expect(send).toHaveBeenCalledTimes(3);
  });

  it("stops listening for bearers once stopped", async () => {
    stop = startAttention(send);
    touch();
    stop();
    await bearer.store("tok");
    expect(send).not.toHaveBeenCalled();
  });
});
