// The drawer (docs/web-client.md): everything about a session that is not the
// conversation — what the session is, how it runs, its policy, its background
// tasks and inbox — behind one control in the top bar, so the page itself is
// the transcript and the composer. A side sheet on a wide window, a bottom
// sheet on a phone; the slide is the inspector's (transform only, entry from
// @starting-style, exit via `.closing` with the unmount on transitionend).
//
// A section with nothing in it is not drawn: "no processes" and "nothing
// pending" were placeholders a reader had to open to learn nothing from.

import { type ReactNode, useState } from "react";
import { patchLoop } from "../api/http.ts";
import type {
  Pace,
  PendingNotification,
  Proposal,
  ReviewLevel,
  Role,
  SessionIdentity,
  Usage,
} from "../api/types.ts";
import { homeRelative } from "./identity.ts";
import { Inbox, Policy } from "./Panel.tsx";
import { useDragToDismiss } from "./useDragToDismiss.ts";
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
  notifications,
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
  notifications: PendingNotification[];
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
          <section className="drawer-section">
            <SessionFacts identity={identity} usage={usage} />
            <RunControls pace={pace} reviewLevel={reviewLevel} act={act} />
          </section>
          {(role !== null || proposals.length > 0) && (
            <Section title="Policy">
              <Policy role={role} proposals={proposals} act={act} />
            </Section>
          )}
        </div>
      </aside>
    </>
  );
}
