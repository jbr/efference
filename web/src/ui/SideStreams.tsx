import type { SummaryPreviewNode } from "../session/tree.ts";
import type { Streaming } from "../session/store.ts";
import { Markdown } from "./Markdown.tsx";
import { LiveThinking, PinnedBox } from "./Thinking.tsx";
import { useViewSettings } from "./viewSettings.ts";
import "./SideStreams.css";

export function SummaryPreview({ node }: { node: SummaryPreviewNode }) {
  const { thinking } = useViewSettings();
  const extent =
    node.from === 0 ? `through #${node.upto}` : `#${node.from}–#${node.upto}`;
  return (
    <section
      className="summary side-summary"
      aria-label={`${node.source} in progress`}
    >
      <div className="summary-rule">
        <span className="glyph">⊙</span>
        <span className="c-label">
          {node.source === "compaction" ? "compacting" : "consolidating"}{" "}
          {extent}…
        </span>
      </div>
      {thinking && node.thinking && <LiveThinking text={node.thinking} />}
      {node.text ? (
        <div className="entry">
          <div className="label" />
          <div className="content">
            <Markdown>{node.text}</Markdown>
          </div>
        </div>
      ) : (
        !node.thinking && <p className="side-waiting">Writing the account…</p>
      )}
      <div className="summary-rule closing" />
    </section>
  );
}

export function TriagePreview({ tails }: { tails: Streaming["triage"] }) {
  const { thinking } = useViewSettings();
  return (
    <>
      {tails.map((tail) => (
        <section
          className="triage-preview"
          key={JSON.stringify(tail.callIds)}
          aria-label="Triage in progress"
        >
          <div className="t-box-head">
            {tail.callIds.length === 0
              ? "Triaging the idle results…"
              : `Triaging ${tail.callIds.length === 1 ? "the result" : `${tail.callIds.length} results`}…`}
          </div>
          {thinking && tail.thinking && (
            <PinnedBox grows={tail.thinking} className="t-thinking">
              <Markdown>{tail.thinking}</Markdown>
            </PinnedBox>
          )}
          {tail.text && (
            <details className="triage-verdict">
              <summary>Arriving verdict</summary>
              <PinnedBox grows={tail.text}>
                <pre>{tail.text}</pre>
              </PinnedBox>
            </details>
          )}
        </section>
      ))}
    </>
  );
}
