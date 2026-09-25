// Dev-only stream scrubbing: keep every frame the live session delivers, and
// control when it folds (docs/web-client.md).
//
// The problem is that a pre-commit state is real but momentary. A tool call's
// input arrives over a few hundred milliseconds and is then superseded by the
// committed entry, and looking at what was on screen during those milliseconds
// means catching them — which a screenshot taken through an extension, sharing
// the event loop with the render it is trying to observe, does not reliably do.
//
// So this holds the fold rather than slowing the stream. Frames keep arriving
// from the daemon and keep landing on the tape; what stops is the store
// consuming them. The tape is the session's own frames in arrival order, so the
// state at position N is the state the client genuinely passed through — nothing
// here synthesizes a frame, reorders one, or stands in for one.
//
// Scope: within the current session, and in memory. A reload starts a new tape.
// Repeatability across reloads would mean persisting the tape (localStorage) or
// serving a recorded session through the daemon's own code
// (backlog/session-replay.md) — the latter being the only way to get it for the
// TUI too.
//
// Reached through an `import.meta.env.DEV` guard in `useSession.ts`, so a
// production build discards the branch and this module with it.

import type { Frame } from "../api/types.ts";
import type { FrameTap } from "./connect.ts";
import { applyFrame, type Session, type Store } from "./store.ts";

/**
 * How many frames a tape holds before it stops recording.
 *
 * It stops rather than dropping the oldest: every position is reconstructed by
 * folding from the tape's base, so discarding the head would make every
 * remaining position unreachable. A full tape is a legible limit; a silently
 * rebased one is a wrong transcript.
 */
const MAX_TAPE = 20_000;

/** The dev console handle, also driven by [`SCRUB_EVENT`]. */
export interface ScrubHandle {
  /** Frames folded into the store. */
  readonly cursor: number;
  /** Frames on the tape. Keeps growing while held. */
  readonly length: number;
  /** Whether the store is tracking the live end of the tape. */
  readonly following: boolean;
  /** Stop folding; arriving frames keep landing on the tape. */
  hold(): void;
  /** Fold everything held back, and track the live end again. */
  resume(): void;
  /** Fold `n` more frames, staying held. */
  step(n?: number): void;
  /** Put the store at exactly `n` frames from the start of the tape. */
  seek(n: number): void;
}

declare global {
  interface Window {
    __scrub?: ScrubHandle;
  }
}

/**
 * The DOM-event name the handle also answers to:
 *
 *     document.dispatchEvent(new CustomEvent("efference:scrub",
 *       { detail: { op: "seek", n: 137 } }))
 *
 * `op` is `hold` | `resume` | `step` | `seek`; `n` is its argument.
 *
 * `window.__scrub` is reachable from devtools, which evaluates in the page's
 * world. A browser extension is not: its scripts run in an isolated world that
 * shares the DOM and nothing else, so a global the page set is invisible to it.
 * Events cross that boundary — the detail is structured-cloned — and driving the
 * scrub from an extension, then screenshotting, is the workflow this exists for.
 */
export const SCRUB_EVENT = "efference:scrub";

interface ScrubCommand {
  op: "hold" | "resume" | "step" | "seek";
  n?: number;
}

/**
 * Install the scrubber over `store`. The returned tap goes to [`connect`], which
 * calls it instead of folding directly.
 *
 * [`connect`]: ./connect.ts
 */
export function record(store: Store): { tap: FrameTap; close(): void } {
  const recorder = new Recorder(store);
  return {
    tap: recorder,
    close: () => recorder.close(),
  };
}

class Recorder implements FrameTap, ScrubHandle {
  /** Every frame since the last catch-up read, in arrival order. */
  private tape: Frame[] = [];
  /**
   * The store as it stood before the tape's first frame — the seeded transcript
   * window. Every position is `base` plus a prefix of the tape, which is what
   * makes a seek backwards exact rather than approximate.
   */
  private base: Session;
  private full = false;
  cursor = 0;
  following = true;

  constructor(private readonly store: Store) {
    this.base = store.snapshot();
    window.__scrub = this;
    document.addEventListener(SCRUB_EVENT, this.onCommand);
    console.info(
      "[scrub] recording the stream: window.__scrub.hold() / resume() / " +
        "step(n) / seek(n), or dispatch an `efference:scrub` event " +
        "(session/record.ts). Each command logs the position it lands on.",
    );
  }

  get length(): number {
    return this.tape.length;
  }

  /** A fresh catch-up read landed: what came before describes a replaced store. */
  reset(): void {
    this.tape = [];
    this.cursor = 0;
    this.full = false;
    this.following = true;
    this.base = this.store.snapshot();
  }

  frame(frame: Frame): void {
    if (this.tape.length >= MAX_TAPE) {
      if (!this.full) console.warn(`[scrub] ${this.describe()}`);
      this.full = true;
      return;
    }
    this.tape.push(frame);
    if (this.following) this.foldTo(this.tape.length);
  }

  /**
   * Put the store at exactly `n` frames. Forward is folded one frame at a time
   * so the render re-runs per frame, the way it does live — a state reached in
   * one bulk fold can hide a stale-memo bug that only appears when each delta
   * drove its own render. Backwards has no such history to preserve, so it
   * refolds from the base in one pass.
   */
  private foldTo(n: number): void {
    const target = Math.max(0, Math.min(n, this.tape.length));
    if (target < this.cursor) {
      const base = this.base;
      const prefix = this.tape.slice(0, target);
      // `connection` is carried over rather than rewound. It is not a recorded
      // fact — no frame carries it; the connection lifecycle sets it directly —
      // so refolding would restore whatever it was when the base was taken and
      // report `connecting…` over a session that is connected and streaming.
      this.store.update((s) => ({
        ...prefix.reduce(applyFrame, base),
        connection: s.connection,
      }));
      this.cursor = target;
    } else {
      while (this.cursor < target) {
        const frame = this.tape[this.cursor];
        this.store.update((s) => applyFrame(s, frame));
        this.cursor += 1;
      }
    }
    // While held, a scrub is a deliberate move to one position and the render
    // is what is being looked at, so the store notifies now rather than on the
    // next animation frame. While following, the coalescing is the point
    // (`store.ts`): the live path must not render per delta.
    if (!this.following) this.store.flush();
  }

  hold(): void {
    this.following = false;
    this.announce();
  }

  resume(): void {
    this.following = true;
    this.foldTo(this.tape.length);
    this.announce();
  }

  step(n = 1): void {
    this.following = false;
    this.foldTo(this.cursor + n);
    this.announce();
  }

  seek(n: number): void {
    this.following = false;
    this.foldTo(n);
    this.announce();
  }

  /** Name the position a command landed on. Two screenshots of the same
   * render at different cursors are otherwise indistinguishable, so whoever
   * drives the scrub reads the position here rather than off the page. */
  private announce(): void {
    console.info(`[scrub] ${this.describe()}`);
  }

  private onCommand = (event: Event): void => {
    const { op, n } = ((event as CustomEvent).detail ?? {}) as ScrubCommand;
    switch (op) {
      case "hold":
        return this.hold();
      case "resume":
        return this.resume();
      case "step":
        return this.step(n);
      case "seek":
        return this.seek(n ?? 0);
    }
  };

  close(): void {
    document.removeEventListener(SCRUB_EVENT, this.onCommand);
    // Only if it is still this recorder's: StrictMode builds a second one before
    // the first one's cleanup lands, and an unconditional delete would take away
    // the live recorder's handle.
    if (window.__scrub === this) delete window.__scrub;
  }

  describe(): string {
    const full = this.full ? ` · tape full (${MAX_TAPE})` : "";
    return this.following
      ? `rec ${this.tape.length}${full}`
      : `scrub ${this.cursor}/${this.tape.length}${full}`;
  }
}
