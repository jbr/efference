// Telling the host whether a person is attending this page (docs/web-push.md).
//
// The host holds one table of clients' latest reports and decides, when a session
// has something to announce, whether anyone is attending. A report is
// `{client, session, active}`: `client` is a per-page-load id, `session` the
// session the page shows (null on the list), `active` whether the page is
// visible, focused, and has seen input recently. `Attender` is the decision
// of when a report is due, with every input passed in so it runs without a
// browser; `startAttention` wires it to the page's events.

import { sendAttention } from "./api/http.ts";
import { sessionId } from "./api/base.ts";
import { bearer } from "./api/bearer.ts";
import type { AttentionReport } from "./api/types.ts";
import { subscribe as subscribeRoute } from "./ui/route.ts";

/** How recent an input event must be for the person to count as attending. */
export const INPUT_WINDOW_MS = 60_000;

/** The longest an active page goes without reporting. The host lets a client
 * lapse 45 s after its last report (`LAPSE`, src/session/attention.rs), so two
 * beats fit inside it. */
export const HEARTBEAT_MS = 20_000;

/** How often the page re-evaluates with no event: the only way the lapse of the
 * input window (active to inactive) is noticed, since idleness fires nothing. */
export const TICK_MS = 5_000;

/** The least gap between two reports that were caused by input alone. Input
 * moves a report only when it turns an inactive page active, and the window
 * above bounds how often that can happen; this keeps a pointer that flickers
 * across the window edge from reporting on each flicker. */
export const INPUT_THROTTLE_MS = 10_000;

export interface Attending {
  visible: boolean;
  focused: boolean;
  /** When the last input event arrived, on the same clock as `now`. */
  lastInputAt: number | null;
  now: number;
}

/** Whether a person is attending: the page is visible and focused and has seen
 * input within the window. */
export function isActive({
  visible,
  focused,
  lastInputAt,
  now,
}: Attending): boolean {
  return (
    visible &&
    focused &&
    lastInputAt !== null &&
    now - lastInputAt < INPUT_WINDOW_MS
  );
}

/**
 * The state machine behind reporting. Feed it the page's state; `due(now)` says
 * whether a report should go out now, and records it as sent when it does.
 *
 * A report is due when `active` or `session` differs from the last one sent, and
 * when the page is active and the last report is a heartbeat old. A change to
 * inactive is never throttled; a change to active is throttled to one per
 * [`INPUT_THROTTLE_MS`] so the edge of the input window cannot flap.
 */
export class Attender {
  visible = false;
  focused = false;
  session: string | null = null;
  lastInputAt: number | null = null;
  private sent: { active: boolean; session: string | null; at: number } | null =
    null;

  constructor(readonly client: string) {}

  input(now: number) {
    this.lastInputAt = now;
  }

  due(now: number): AttentionReport | null {
    const active = isActive({
      visible: this.visible,
      focused: this.focused,
      lastInputAt: this.lastInputAt,
      now,
    });
    const last = this.sent;
    if (last !== null) {
      const changed = active !== last.active || this.session !== last.session;
      if (!changed && !(active && now - last.at >= HEARTBEAT_MS)) return null;
      if (
        changed &&
        active &&
        !last.active &&
        this.session === last.session &&
        now - last.at < INPUT_THROTTLE_MS
      ) {
        return null;
      }
    } else if (!active && this.session === null) {
      // Nothing has been said and nothing is worth saying: the host treats a
      // client it has not heard from as inattentive already.
      return null;
    }
    this.sent = { active, session: this.session, at: now };
    return { client: this.client, session: this.session, active };
  }
}

/** A per-page-load id. `randomUUID` needs a secure context; the fallback covers a
 * plain-http origin. */
export function newClientId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  return Array.from({ length: 4 }, () =>
    Math.floor(Math.random() * 2 ** 32)
      .toString(16)
      .padStart(8, "0"),
  ).join("");
}

const INPUT_EVENTS = [
  "pointermove",
  "pointerdown",
  "keydown",
  "wheel",
  "scroll",
  "touchstart",
] as const;

/**
 * Start reporting for this page load; returns the function that stops it. A
 * failed report is logged and not shown. A `404` means the endpoint is a lone
 * session daemon, which serves no host table, so reporting stops. While the page
 * holds no bearer (api/bearer.ts) nothing is sent, since the host would refuse
 * it, and a bearer being stored is a moment to report.
 */
export function startAttention(
  send: (report: AttentionReport) => Promise<void> = sendAttention,
): () => void {
  const attender = new Attender(newClientId());
  let stopped = false;

  const flush = () => {
    if (stopped || bearer.held() === null) return;
    attender.visible = document.visibilityState === "visible";
    attender.focused = document.hasFocus();
    attender.session = sessionId();
    const report = attender.due(performance.now());
    if (report === null) return;
    send(report).catch((error: unknown) => {
      console.warn("attention report failed:", error);
      if ((error as { status?: number }).status === 404) stop();
    });
  };
  const onInput = () => {
    attender.input(performance.now());
    flush();
  };

  const options = { capture: true, passive: true } as const;
  for (const name of INPUT_EVENTS)
    window.addEventListener(name, onInput, options);
  window.addEventListener("focus", flush);
  window.addEventListener("blur", flush);
  window.addEventListener("pagehide", flush);
  document.addEventListener("visibilitychange", flush);
  const unroute = subscribeRoute(flush);
  const unbearer = bearer.subscribe(flush);
  const timer = setInterval(flush, TICK_MS);

  function stop() {
    stopped = true;
    for (const name of INPUT_EVENTS) {
      window.removeEventListener(name, onInput, options);
    }
    window.removeEventListener("focus", flush);
    window.removeEventListener("blur", flush);
    window.removeEventListener("pagehide", flush);
    document.removeEventListener("visibilitychange", flush);
    unroute();
    unbearer();
    clearInterval(timer);
  }
  flush();
  return stop;
}
