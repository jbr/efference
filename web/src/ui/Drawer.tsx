// The drawer (docs/web-client.md): everything about a session that is not the
// conversation — what the session is, how it runs, its policy, its background
// tasks and inbox — behind one control in the top bar, so the page itself is
// the transcript and the composer. A side sheet on a wide window, a bottom
// sheet on a phone; the slide is the inspector's (transform only, entry from
// @starting-style, exit via `.closing` with the unmount on transitionend).
//
// A section with nothing in it is not drawn: "no processes" and "nothing
// pending" were placeholders a reader had to open to learn nothing from.

import { type PointerEvent, type ReactNode, useRef, useState } from "react";
import { patchLoop } from "../api/http.ts";
import type {
  Pace,
  PendingNotification,
  Process,
  Proposal,
  ReviewLevel,
  Role,
  SessionIdentity,
  Usage,
} from "../api/types.ts";
import { homeRelative } from "./identity.ts";
import { Inbox, Interface, Policy, Tasks } from "./Panel.tsx";
import { useNarrow } from "./useNarrow.ts";
import "./Drawer.css";

const PACES: [number, string][] = [
  [0, "No delay"],
  [2000, "2 seconds"],
  [10000, "10 seconds"],
  [30000, "30 seconds"],
];

/** The review dial in words (docs/permissions.md): who answers a call the
 * policy does not clear. `upstream` is offered only while it is the value —
 * it names a relation this client cannot set up. */
const LEVELS: [ReviewLevel, string][] = [
  ["autonomous", "The spotter decides"],
  ["spotter_human", "The spotter, then you"],
  ["human_only", "You review every call"],
  ["upstream", "The supervising session"],
];

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="drawer-section">
      <h2 className="drawer-heading">{title}</h2>
      {children}
    </section>
  );
}

function SessionFacts({
  identity,
  usage,
}: {
  identity: SessionIdentity | null;
  usage: Usage | null;
}) {
  const inference = identity?.inference;
  const triage =
    inference &&
    !inference.triage.shared &&
    inference.triage.model !== inference.main.model
      ? inference.triage
      : null;
  return (
    <dl className="facts">
      {identity !== null && (
        <>
          <dt>model</dt>
          <dd>
            {inference
              ? `${inference.main.connection}/${identity.model}`
              : identity.model}
          </dd>
        </>
      )}
      {inference && (
        <>
          <dt>spotter</dt>
          <dd>
            {inference.spotter.connection}/{inference.spotter.model}
          </dd>
        </>
      )}
      {triage && (
        <>
          <dt>triage</dt>
          <dd>
            {triage.connection}/{triage.model}
          </dd>
        </>
      )}
      {identity !== null && (
        <>
          <dt>directory</dt>
          <dd title={identity.cwd}>{homeRelative(identity.cwd)}</dd>
        </>
      )}
      <dt>session</dt>
      <dd>{identity?.id}</dd>
      {usage?.cost && (
        <>
          <dt>cost</dt>
          <dd>
            ${usage.cost.usd.toFixed(2)}
            {usage.cost.unpriced > 0 && ` (+${usage.cost.unpriced} unpriced)`}
          </dd>
        </>
      )}
    </dl>
  );
}

function RunControls({
  pace,
  reviewLevel,
  act,
}: {
  pace: Pace | null;
  reviewLevel: ReviewLevel | null;
  act: (call: Promise<unknown>) => void;
}) {
  const speed = pace?.speed_ms ?? 0;
  const held = pace?.held ?? false;
  return (
    <div className="drawer-controls">
      <label className="drawer-control">
        <span>Review</span>
        <select
          value={reviewLevel ?? ""}
          onChange={(e) =>
            act(patchLoop({ reviewLevel: e.target.value as ReviewLevel }))
          }
        >
          {reviewLevel === null && <option value="" disabled hidden />}
          {LEVELS.filter(
            ([level]) => level !== "upstream" || reviewLevel === "upstream",
          ).map(([level, label]) => (
            <option key={level} value={level}>
              {label}
            </option>
          ))}
        </select>
      </label>
      <label className="drawer-control">
        <span>Pause between steps</span>
        <select
          value={speed}
          onChange={(e) => act(patchLoop({ speedMs: Number(e.target.value) }))}
        >
          {!PACES.some(([ms]) => ms === speed) && (
            <option value={speed}>{speed} ms</option>
          )}
          {PACES.map(([ms, label]) => (
            <option key={ms} value={ms}>
              {label}
            </option>
          ))}
        </select>
      </label>
      <label className="drawer-control">
        <span>Hold after the current step</span>
        <input
          type="checkbox"
          checked={held}
          onChange={() => act(patchLoop({ held: !held }))}
        />
      </label>
    </div>
  );
}

/** How far past the release point a flick is taken to carry the sheet, in ms
 * of its release velocity. iOS projects a released sheet's momentum and
 * settles on the side of the midpoint the projection lands on; this is that
 * rule with a constant deceleration window. */
const PROJECTION_MS = 180;

/** Only the last stretch of a drag is its release velocity: a drag that sped
 * up and then slowed to a stop was not a flick. */
const VELOCITY_WINDOW_MS = 80;

/** Drag the phone sheet down by its header to dismiss it — the grabber's
 * promise. The sheet follows the finger; on release it closes if where its
 * momentum would carry it is past half its height, and otherwise springs
 * back. The transform is set on the element directly so a drag re-renders
 * nothing. */
function useDragToDismiss(onClose: () => void) {
  const sheet = useRef<HTMLElement>(null);
  const drag = useRef<{ y: number; samples: [number, number][] } | null>(null);
  const offset = (event: PointerEvent) =>
    Math.max(0, event.clientY - (drag.current?.y ?? event.clientY));
  const onPointerDown = (event: PointerEvent) => {
    if ((event.target as HTMLElement).closest("button")) return;
    drag.current = { y: event.clientY, samples: [[event.timeStamp, 0]] };
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    if (sheet.current) sheet.current.style.transition = "none";
  };
  const onPointerMove = (event: PointerEvent) => {
    if (!drag.current || !sheet.current) return;
    const dy = offset(event);
    const samples = drag.current.samples;
    samples.push([event.timeStamp, dy]);
    // Above its resting place the sheet stretches with resistance rather
    // than stopping dead, the way an iOS sheet does when pushed back up.
    const raw = event.clientY - drag.current.y;
    const shown = raw < 0 ? -Math.sqrt(-raw) * 2 : dy;
    while (
      samples.length > 2 &&
      event.timeStamp - samples[0][0] > VELOCITY_WINDOW_MS
    ) {
      samples.shift();
    }
    sheet.current.style.transform = `translateY(${shown}px)`;
  };
  const onPointerUp = (event: PointerEvent) => {
    const state = drag.current;
    const element = sheet.current;
    const dy = offset(event);
    drag.current = null;
    if (!state || !element) return;
    const [t0, y0] = state.samples[0];
    const velocity = (dy - y0) / Math.max(1, event.timeStamp - t0);
    element.style.transition = "";
    element.style.transform = "";
    if (dy + velocity * PROJECTION_MS > element.offsetHeight / 2) onClose();
  };
  return {
    sheet,
    handlers: {
      onPointerDown,
      onPointerMove,
      onPointerUp,
      onPointerCancel: onPointerUp,
    },
  };
}

export function Drawer({
  closing,
  onSlideOut,
  onClose,
  identity,
  usage,
  pace,
  reviewLevel,
  role,
  proposals,
  processes,
  notifications,
  processOutput,
  processLost,
}: {
  /** The exit transition is running; `onSlideOut` unmounts when it lands. */
  closing: boolean;
  onSlideOut: () => void;
  onClose: () => void;
  identity: SessionIdentity | null;
  usage: Usage | null;
  pace: Pace | null;
  reviewLevel: ReviewLevel | null;
  role: Role | null;
  proposals: Proposal[];
  processes: Process[];
  notifications: PendingNotification[];
  processOutput: Record<number, string>;
  processLost: Record<number, number>;
}) {
  const [error, setError] = useState<string | null>(null);
  const narrow = useNarrow();
  const { sheet, handlers } = useDragToDismiss(onClose);
  const act = (call: Promise<unknown>) =>
    void call.then(() => setError(null)).catch((e) => setError(`${e}`));
  return (
    <>
      <div
        className={closing ? "drawer-scrim closing" : "drawer-scrim"}
        onClick={onClose}
      />
      <aside
        ref={sheet}
        className={closing ? "drawer closing" : "drawer"}
        aria-label="session details"
        onTransitionEnd={(event) => {
          if (event.target === event.currentTarget && closing) onSlideOut();
        }}
      >
        <header className="drawer-head" {...(narrow ? handlers : {})}>
          <span className="drawer-grabber" aria-hidden />
          <h1 className="drawer-title">Session details</h1>
          <button className="drawer-close" aria-label="close" onClick={onClose}>
            Done
          </button>
        </header>
        <div className="drawer-body">
          {identity?.title !== undefined && (
            <p className="drawer-session-title">{identity.title}</p>
          )}
          {error && <div className="error">{error}</div>}
          {notifications.length > 0 && (
            <Section title="Inbox">
              <Inbox notifications={notifications} act={act} />
            </Section>
          )}
          {processes.length > 0 && (
            <Section title="Tasks">
              <Tasks
                processes={processes}
                output={processOutput}
                lost={processLost}
                act={act}
              />
            </Section>
          )}
          <section className="drawer-section">
            <SessionFacts identity={identity} usage={usage} />
            <RunControls pace={pace} reviewLevel={reviewLevel} act={act} />
          </section>
          {(role !== null || proposals.length > 0) && (
            <Section title="Policy">
              <Policy role={role} proposals={proposals} act={act} />
            </Section>
          )}
          <Section title="This device">
            <Interface />
          </Section>
        </div>
      </aside>
    </>
  );
}
