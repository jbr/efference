import { useState } from "react";
import type { UsageView } from "../api/types.ts";
import { BundleNoticeStrip } from "../ui/BundleNotice.tsx";
import { MoreButton, Tab } from "../ui/SessionSwitcher.tsx";
import { TopBar } from "../ui/TopBar.tsx";
import { tabCards } from "./fixtures.ts";
import { Specimen } from "./Specimens.tsx";
import { SessionStatusFixture } from "./SessionStatusFixture.tsx";

const usage: UsageView = {
  last: null,
  cumulative: {
    input_tokens: 147_000,
    output_tokens: 2400,
    cache_read_input_tokens: 120_000,
    cache_creation_input_tokens: 0,
  },
  last_request_at: null,
  context: { used: 147_000, limit: 272_000 },
  cost: { usd: 0.42, unpriced: 0 },
  cold_streak: 0,
  triage_cold_streak: null,
  last_cache_miss: null,
  keepalive_skipped: null,
  triage_keepalive_skipped: null,
  plan: null,
  triage_plan: null,
  retention: { kept_lines: 0, shown_lines: 0, truncated_lines: 0, withheld: 0 },
};

/** Real header controls and tab components, without a host subscription. */
export function GalleryTranscriptHeader() {
  return (
    <div className="g-transcript-header">
      <TopBar
        listHref={null}
        identity={{
          id: "deft-walnut",
          title: "Worktree controls in the web UI",
          model: "gpt-6.1",
          cwd: "/Users/jbr/code/efference",
        }}
        usage={usage}
        upstreamFailure={null}
        upstreamRetry={null}
      />
      <nav className="switcher" aria-label="Example session tabs">
        {tabCards.slice(0, 5).map((card) => (
          <span key={card.id} className="switcher-slot">
            <Tab card={card} inert current={card.id === "deft-walnut"} />
          </span>
        ))}
        <MoreButton total={3} waiting={1} onClick={() => {}} />
      </nav>
    </div>
  );
}

const statusStates = [
  "Your turn",
  "Environment",
  "Mounting MCP",
  "Authorization",
  "Startup failed",
  "Retry refused",
  "Reconnecting",
  "Inspection failed",
] as const;
export type StatusState = (typeof statusStates)[number];

export function SessionChromeSpecimens() {
  const [state, setState] = useState<StatusState>("Mounting MCP");
  return (
    <>
      <p className="g-intro">
        Session chrome using the production header, tabs, and composer status.
        Errors take the status slot without stopping an ongoing turn. Tabs and
        overflow are inert fixtures; settings and meters use their real
        controls. Retry demonstrates the disabled, in-flight state.
      </p>
      <div
        className="g-scene-modes"
        role="group"
        aria-label="Session status state"
      >
        {statusStates.map((value) => (
          <button
            key={value}
            aria-pressed={state === value}
            onClick={() => setState(value)}
          >
            {value}
          </button>
        ))}
      </div>
      <Specimen name="stale bundle notice">
        <div className="g-session-chrome">
          <BundleNoticeStrip onReload={() => {}} />
          <div className="g-chrome-body g-caption">
            Shown when the host serves a build newer than the one this page
            loaded (api/bundle.ts). The reload is a button, not a timer: only
            the person knows when the page is theirs to lose.
          </div>
        </div>
      </Specimen>
      <Specimen name="transcript header + composer status">
        <div className="g-session-chrome">
          <GalleryTranscriptHeader />
          <div className="g-chrome-body g-caption">
            No status at the top. Progress and failures occupy the composer
            readout below; the draft stays editable. Inspection failed takes the
            status slot even while the turn continues. All actions are
            gallery-only.
          </div>
          <SessionStatusFixture key={state} state={state} />
        </div>
      </Specimen>
    </>
  );
}
