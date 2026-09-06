// The scrubber's tape semantics. The tape holds the session's own frames, so
// what these pin is the addressing: position N is the state the client passed
// through after N frames, reachable from either direction and identical both
// ways. `store.test.ts` covers the fold itself.
//
// jsdom rather than the suite's default node environment: the recorder installs
// a corner badge and a document-level command listener, both of which are how it
// is driven, so a test that stubbed them out would be testing something else.

// @vitest-environment jsdom

import { beforeEach, describe, expect, it } from "vitest";
import type { Frame } from "../api/types.ts";
import { record, SCRUB_EVENT } from "./record.ts";
import { Store } from "./store.ts";
import { messagesAssistant } from "./assistant.ts";

/** One `tool_call_input` delta — the frame class the scrubber exists for. */
const fragment = (index: number, text: string): Frame => ({
  class: "output",
  kind: "tool_call_input",
  data: { index, fragment: text },
});

const commit: Frame = {
  class: "log",
  kind: "entry",
  seq: 9,
  data: {
    seq: 9,
    turn: 1,
    at: 0,
    payload: {
      Assistant: messagesAssistant(
        [],
        [],
        [{ id: "c1", name: "write", input: { path: "a.rs" } }],
      ),
    },
  },
};

const start: Frame = {
  class: "output",
  kind: "tool_call_start",
  data: { index: 0, call_id: "c1", name: "write" },
};

const arriving = [
  start,
  fragment(0, '{"path"'),
  fragment(0, ':"a.rs"'),
  fragment(0, "}"),
  commit,
];

function installed() {
  const store = new Store();
  const { tap, close } = record(store);
  tap.reset();
  return { store, tap, close, scrub: () => window.__scrub! };
}

beforeEach(() => {
  document.body.replaceChildren();
});

describe("the tape", () => {
  it("folds as frames arrive while following", () => {
    const { store, tap, close, scrub } = installed();
    for (const frame of arriving) tap.frame(frame);
    expect(scrub().cursor).toBe(5);
    expect(scrub().length).toBe(5);
    expect(store.snapshot().lastSeq).toBe(9);
    close();
  });

  it("keeps recording while held, without folding", () => {
    const { store, tap, close, scrub } = installed();
    tap.frame(arriving[0]);
    tap.frame(arriving[1]);
    scrub().hold();
    for (const frame of arriving.slice(2)) tap.frame(frame);
    // The tape has everything; the store stopped where the hold landed.
    expect(scrub().length).toBe(5);
    expect(scrub().cursor).toBe(2);
    expect(store.snapshot().streaming.blocks[0].text).toBe('{"path"');
    close();
  });

  it("catches up on resume", () => {
    const { store, tap, close, scrub } = installed();
    scrub().hold();
    for (const frame of arriving) tap.frame(frame);
    expect(store.snapshot().lastSeq).toBeNull();
    scrub().resume();
    expect(scrub().cursor).toBe(5);
    expect(store.snapshot().lastSeq).toBe(9);
    close();
  });

  it("reaches a position identically forwards and backwards", () => {
    // The property the whole affordance rests on: a seek back to 3 is the same
    // state a step forward to 3 was, so a render change can be compared against
    // the same input.
    const { store, tap, close, scrub } = installed();
    scrub().hold();
    for (const frame of arriving) tap.frame(frame);
    scrub().seek(3);
    const forwards = store.snapshot();
    scrub().seek(5);
    scrub().seek(3);
    expect(store.snapshot()).toEqual(forwards);
    close();
  });

  it("does not rewind the connection indicator", () => {
    // `connection` is set by the connection lifecycle, not by any frame, so a
    // refold from the base would restore whatever it was when the base was
    // taken — reporting `connecting…` over a session that is streaming.
    const { store, tap, close, scrub } = installed();
    for (const frame of arriving) tap.frame(frame);
    store.update((s) => ({ ...s, connection: "live" }));
    scrub().seek(2);
    expect(store.snapshot().connection).toBe("live");
    close();
  });

  it("clamps a seek past either end", () => {
    const { tap, close, scrub } = installed();
    for (const frame of arriving) tap.frame(frame);
    scrub().seek(99);
    expect(scrub().cursor).toBe(5);
    scrub().seek(-4);
    expect(scrub().cursor).toBe(0);
    close();
  });

  it("drops the tape when a fresh catch-up read reseeds", () => {
    // The recorded frames describe a store the re-seed has replaced, so keeping
    // them would make every position before the reset wrong rather than old.
    const { tap, close, scrub } = installed();
    for (const frame of arriving) tap.frame(frame);
    tap.reset();
    expect(scrub().length).toBe(0);
    expect(scrub().following).toBe(true);
    close();
  });
});

describe("the command bridge", () => {
  // An extension's scripts run in an isolated world and cannot see
  // `window.__scrub`; DOM events are what crosses that boundary.
  const send = (op: string, n?: number) =>
    document.dispatchEvent(new CustomEvent(SCRUB_EVENT, { detail: { op, n } }));

  it("drives the same operations as the handle", () => {
    const { tap, close, scrub } = installed();
    for (const frame of arriving) tap.frame(frame);
    send("seek", 2);
    expect(scrub().cursor).toBe(2);
    send("step", 2);
    expect(scrub().cursor).toBe(4);
    send("resume");
    expect(scrub().cursor).toBe(5);
    expect(scrub().following).toBe(true);
    send("hold");
    expect(scrub().following).toBe(false);
    close();
  });

  it("stops answering once closed", () => {
    const { tap, close, scrub } = installed();
    for (const frame of arriving) tap.frame(frame);
    const handle = scrub();
    close();
    send("seek", 1);
    expect(handle.cursor).toBe(5);
    expect(window.__scrub).toBeUndefined();
  });
});

describe("the badge", () => {
  it("names the mode and the position", () => {
    const { tap, close, scrub } = installed();
    const badge = () =>
      document.querySelector("[data-scrub-badge]")?.textContent;
    for (const frame of arriving) tap.frame(frame);
    expect(badge()).toBe("rec 5");
    scrub().seek(2);
    expect(badge()).toBe("scrub 2/5");
    close();
    expect(badge()).toBeUndefined();
  });
});
