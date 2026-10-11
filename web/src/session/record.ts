// Dev-only rolling stream recording and controlled folding (docs/web-client.md).
// Retained frames replay from their exact base; a held store snapshot remains
// independent when its position leaves the retained window.

import type { Frame } from "../api/types.ts";
import type { FrameTap } from "./connect.ts";
import { applyFrame, type Session, type Store } from "./store.ts";
import { sessionId } from "../api/base.ts";
import { listTapes, loadTape, saveTape } from "./tapeStore.ts";
import { FrameTape } from "./frameTape.ts";

/** Number of recent frames retained; older frames fold into the replay base. */
const MAX_TAPE = 20_000;

/** The dev console handle, also driven by [`SCRUB_EVENT`]. */
export interface ScrubHandle {
  /** Frames folded into the store. */
  readonly cursor: number;
  /** Frames retained on the rolling tape. */
  readonly length: number;
  /** Whether the store is tracking the live end of the tape. */
  readonly following: boolean;
  /** The displayed held snapshot predates the retained replay window. */
  readonly expired: boolean;
  /** Frames already folded into the rolling base. */
  readonly evicted: number;
  /** Stop folding; arriving frames keep landing on the tape. */
  hold(): void;
  /** Fold everything held back, and track the live end again. */
  resume(): void;
  /** Fold `n` more frames, staying held. */
  step(n?: number): void;
  /** Put the store at exactly `n` frames from the start of the tape. */
  seek(n: number): void;
  /** Replace the live tape with a saved one (`session/tapeStore.ts`) — the
   * newest earlier recording of this session when `key` is omitted — held at
   * its end. Live frames stop landing on the tape: a replay is not mixed with
   * the stream. Reload to record again. */
  restore(key?: string): Promise<void>;
  /** The saved recordings of this session, newest first. */
  tapes(): Promise<{ key: string; startedAt: number; frames: number }[]>;
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
  op: "hold" | "resume" | "step" | "seek" | "restore";
  n?: number;
  /** For `restore`: which saved recording; the newest earlier one if absent. */
  key?: string;
}

/** Recorders not yet closed, oldest first. A reload or an HMR update can
 * leave two alive at once for a moment; the console handle is always the
 * newest still open, so closing one never strands the other without it. */
const open: Recorder[] = [];

/**
 * Install the scrubber over `store`. The returned tap goes to [`connect`], which
 * calls it instead of folding directly.
 *
 * [`connect`]: ./connect.ts
 */
export function record(
  store: Store,
  capacity = MAX_TAPE,
): { tap: FrameTap; scrub: ScrubHandle; close(): void } {
  const recorder = new Recorder(store, capacity);
  return {
    tap: recorder,
    scrub: recorder,
    close: () => recorder.close(),
  };
}

class Recorder implements FrameTap, ScrubHandle {
  /** Recent frames in arrival order, independent of a held store snapshot. */
  private tape: FrameTape;
  /**
   * The state before the first retained frame: the seed plus evicted frames.
   * Every position is `base` plus a prefix of the tape, so backwards seeks
   * reconstruct an exact state.
   */
  private base: Session;
  expired = false;
  evicted = 0;
  cursor = 0;
  following = true;
  /** Whose recording this is, and when it began — its key in the saved-tape
   * store. A catch-up read starts a new recording under a new key. */
  private readonly session = sessionId() ?? "root";
  private startedAt = Date.now();
  /** A restored replay is playing: live frames are not recorded over it. */
  private replaying = false;
  private saveTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(
    private readonly store: Store,
    private readonly capacity: number,
  ) {
    this.tape = new FrameTape(capacity);
    this.base = store.snapshot();
    open.push(this);
    window.__scrub = this;
    document.addEventListener(SCRUB_EVENT, this.onCommand);
    console.info(
      "[scrub] recording the stream: window.__scrub.hold() / resume() / " +
        "step(n) / seek(n) / restore() / tapes(), or dispatch an " +
        "`efference:scrub` event (session/record.ts). Recordings persist " +
        "across reloads; restore() replays the last one. Each command logs " +
        "the position it lands on.",
    );
  }

  get length(): number {
    return this.tape.length;
  }

  /** A fresh catch-up read landed: what came before describes a replaced store. */
  reset(): void {
    if (this.replaying) return;
    this.tape = new FrameTape(this.capacity);
    this.cursor = 0;
    this.expired = false;
    this.evicted = 0;
    this.following = true;
    this.base = this.store.snapshot();
    this.startedAt = Date.now();
    this.streamed = false;
  }

  /** Whether the tape holds anything streamed. A connect alone lands a state
   * snapshot on the tape; saving those would push the recordings worth
   * returning to out of the store within a few reloads. */
  private streamed = false;

  /** Save the recording, at most once a second while frames arrive — once it
   * holds streamed output. */
  private scheduleSave(): void {
    if (!this.streamed || this.saveTimer !== null) return;
    // Two recorders overlap across a reload; only the newest records to the
    // store, or every recording is saved twice.
    if (open.at(-1) !== this) return;
    this.saveTimer = setTimeout(() => {
      this.saveTimer = null;
      if (this.replaying || this.tape.length === 0) return;
      void saveTape({
        key: `${this.session}:${this.startedAt}`,
        session: this.session,
        startedAt: this.startedAt,
        base: this.base,
        tape: this.tape.snapshot(),
      }).catch((error: unknown) =>
        console.warn("[scrub] could not save the tape", error),
      );
    }, 1000);
  }

  async tapes() {
    return listTapes(this.session);
  }

  async restore(key?: string): Promise<void> {
    const own = `${this.session}:${this.startedAt}`;
    const chosen =
      key ?? (await listTapes(this.session)).find((t) => t.key !== own)?.key;
    const saved = chosen === undefined ? undefined : await loadTape(chosen);
    if (saved === undefined) {
      console.warn("[scrub] no saved recording to restore");
      return;
    }
    this.replaying = true;
    this.following = false;
    this.base = saved.base;
    this.tape = new FrameTape(Math.max(this.capacity, saved.tape.length));
    for (const frame of saved.tape) this.tape.push(frame);
    this.expired = false;
    this.evicted = 0;
    // Past the end, so foldTo refolds from the restored base rather than
    // walking forward from a cursor into the live tape.
    this.cursor = saved.tape.length + 1;
    this.foldTo(saved.tape.length);
    console.info(
      `[scrub] restored ${chosen} (${saved.tape.length} frames); live frames are not recorded until reload`,
    );
    this.announce();
  }

  frame(frame: Frame): void {
    if (this.replaying) return;
    const evicted = this.tape.push(frame);
    if (evicted !== undefined) {
      this.base = applyFrame(this.base, evicted);
      this.evicted += 1;
      if (this.cursor > 0) this.cursor -= 1;
      else this.expired = true;
    }
    if (frame.class === "output") this.streamed = true;
    this.scheduleSave();
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
    if (this.expired || target < this.cursor) {
      const base = this.base;
      const prefix = this.tape.snapshot(target);
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
        const frame = this.tape.at(this.cursor);
        this.store.update((s) => applyFrame(s, frame));
        this.cursor += 1;
      }
    }
    this.expired = false;
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
    const { op, n, key } = ((event as CustomEvent).detail ??
      {}) as ScrubCommand;
    switch (op) {
      case "hold":
        return this.hold();
      case "resume":
        return this.resume();
      case "step":
        return this.step(n);
      case "seek":
        return this.seek(n ?? 0);
      case "restore":
        return void this.restore(key);
    }
  };

  close(): void {
    if (this.saveTimer !== null) clearTimeout(this.saveTimer);
    document.removeEventListener(SCRUB_EVENT, this.onCommand);
    const at = open.indexOf(this);
    if (at !== -1) open.splice(at, 1);
    if (window.__scrub === this) {
      const newest = open.at(-1);
      if (newest) window.__scrub = newest;
      else delete window.__scrub;
    }
  }

  describe(): string {
    const rolled = this.evicted > 0 ? ` · rolled ${this.evicted}` : "";
    const expired = this.expired ? " · held position expired" : "";
    return this.following
      ? `rec ${this.tape.length}${rolled}`
      : `scrub ${this.cursor}/${this.tape.length}${rolled}${expired}`;
  }
}
