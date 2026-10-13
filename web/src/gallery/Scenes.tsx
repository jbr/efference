import { useMemo, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import type { Ask, CallId, LogEntry } from "../api/types.ts";
import { messagesAssistant } from "../session/assistant.ts";
import { emptyStreaming, type Streaming } from "../session/store.ts";
import { buildTree } from "../session/tree.ts";
import { GateControls } from "../ui/Gate.tsx";
import { ComposerToolbar } from "../ui/ComposerToolbar.tsx";
import { useNarrow } from "../ui/useNarrow.ts";
import { TranscriptContent } from "../ui/Transcript.tsx";
import { CallBlock, ResultBlock, ZoomableImage } from "../ui/ToolBlocks.tsx";
import { turnState, type TurnState } from "../ui/turnState.ts";

const noop = () => {};
const entry = (seq: number, payload: LogEntry["payload"]): LogEntry => ({
  seq,
  turn: 1,
  at: 1_760_000_000_000,
  payload,
});
const command = {
  id: "gallery-check",
  name: "bash",
  input: {
    intent: "Check the web client before committing the worktree controls",
    command: "cd web && pnpm run check",
  },
};
const question =
  "The worktree controls are ready. Should I commit this change now, or leave it uncommitted for another design pass?";
const ask: Ask = {
  call_id: "gallery-ask",
  question,
  n: 1,
  of: 2,
  recalled: [],
};
const secondQuestion =
  "Should the new-session form remember the worktree option for the next session, or reset it each time?";
const secondAsk: Ask = {
  ...ask,
  call_id: "gallery-ask-second",
  question: secondQuestion,
  n: 2,
};
const common: LogEntry[] = [
  entry(1, {
    User: [
      {
        Text: "Add the worktree option to the new-session form, and show worktree events in the transcript. Keep long branch names readable on mobile.",
      },
    ],
  }),
  entry(2, {
    Assistant: messagesAssistant(
      [],
      [
        {
          Text: "I’ll use the existing settings row and transcript notices, then check the client at both widths.",
        },
      ],
      [],
    ),
  }),
  entry(3, { Assistant: messagesAssistant([], [], [command]) }),
];
const result = entry(4, {
  ToolResult: {
    call_id: command.id,
    raw: {
      content:
        "> efference-web check\n> tsc --noEmit\n> eslint .\nAll matched files use Prettier code style!\nTest Files  60 passed (60)\nTests  472 passed (472)\nDuration  9.73s",
      is_error: false,
    },
    wire: {
      Distilled: {
        keep: { Ranges: [[4, 7]] },
        note: "The web check passed: types, lint, formatting, and 472 tests.",
      },
    },
  },
});
// A shortened exchange based on deft-walnut's worktree UI session, not a replay.
const finished = entry(5, {
  Assistant: messagesAssistant(
    [],
    [
      {
        Text: "The web worktree controls are in place.\n\n- The launch form can start a session in its own checkout.\n- Worktree events render in the transcript, with branch names as code chips.\n- Setup output stays behind a disclosure rather than interrupting the conversation.\n\nThe client checks passed. The remaining availability check is separate from this UI change.\n\n```tsx\n<Settings draft={draft} onDraft={setDraft} />\n```",
      },
    ],
    [],
  ),
});
const asking = entry(5, {
  Assistant: messagesAssistant(
    [],
    [],
    [
      { id: ask.call_id, name: "ask", input: { question } },
      {
        id: secondAsk.call_id,
        name: "ask",
        input: { question: secondQuestion },
      },
    ],
  ),
});
const push = {
  id: "gallery-push",
  name: "bash",
  input: {
    intent: "Publish the worktree UI branch for review",
    command: "git push origin efference/worktree-controls",
  },
};
const modes = [
  "Finished",
  "Input arriving",
  "Working",
  "Answer needed",
  "Approval needed",
] as const;
type Mode = (typeof modes)[number];

function ComposerFixture({
  turn,
  running = false,
  focused = false,
  initial = "",
}: {
  turn: TurnState;
  running?: boolean;
  focused?: boolean;
  initial?: string;
}) {
  const narrow = useNarrow();
  const [held, setHeld] = useState(false);
  const [draft, setDraft] = useState(initial);
  const [notice, setNotice] = useState("");
  return (
    <form
      className="composer g-composer"
      onSubmit={(event) => {
        event.preventDefault();
        setNotice("Gallery only — nothing was sent.");
      }}
    >
      <div className="composer-inner">
        <div className={`composer-input${focused ? " is-focus-within" : ""}`}>
          <div className="composer-text-row">
            <textarea
              aria-label="Gallery draft"
              placeholder={
                turn.text === "waiting for your answer"
                  ? "write your answer…"
                  : "send a turn…"
              }
              rows={3}
              value={draft}
              onChange={(event) => setDraft(event.target.value)}
            />
          </div>
          <ComposerToolbar
            turn={turn}
            narrow={narrow}
            held={held}
            onHoldChange={setHeld}
            running={running}
            onAttach={() => setNotice("Gallery only — no file was uploaded.")}
            onInterrupt={() =>
              setNotice("Gallery only — no session was interrupted.")
            }
          />
        </div>
        {notice && (
          <p className="g-caption" role="status">
            {notice}
          </p>
        )}
      </div>
    </form>
  );
}

export function GalleryScenes() {
  const [mode, setMode] = useState<Mode>("Finished");
  const [answerSecond, setAnswerSecond] = useState(false);
  const [inspection, setInspection] = useState<CallId | null>(null);
  const [gateSlot, setGateSlot] = useState<HTMLDivElement | null>(null);
  const [decision, setDecision] = useState("");
  const [steer, setSteer] = useState("");
  const running = mode !== "Finished";
  const entries = useMemo(
    () =>
      mode === "Finished"
        ? [...common, result, finished]
        : mode === "Answer needed"
          ? [
              ...common,
              result,
              asking,
              ...(answerSecond
                ? [
                    {
                      ...entry(6, {
                        ToolResult: {
                          call_id: ask.call_id,
                          raw: {
                            content:
                              "Leave it uncommitted for another design pass.",
                            is_error: false,
                          },
                          wire: "Raw",
                        },
                      }),
                      submission_id: "gallery-answer-first",
                    },
                  ]
                : []),
            ]
          : mode === "Approval needed"
            ? [
                ...common,
                result,
                finished,
                entry(6, { Assistant: messagesAssistant([], [], [push]) }),
                entry(7, {
                  Spot: {
                    call_id: push.id,
                    stance: "gate",
                    reason:
                      "Publishing this branch changes the remote repository. This command needs your approval.",
                    origin: "Model",
                  },
                }),
              ]
            : common,
    [mode, answerSecond],
  );
  const streaming: Streaming =
    mode === "Input arriving"
      ? {
          ...emptyStreaming,
          blocks: [
            {
              index: 0,
              kind: "call",
              callId: "gallery-arriving",
              name: "playwright__browser_run_code_unsafe",
              text: '{"intent":"Check the phone layout before publishing the controls","code":"async page => { await page.setViewportSize({width:390,height',
            },
          ],
        }
      : mode === "Working"
        ? {
            ...emptyStreaming,
            toolOutput: {
              [command.id]:
                "> efference-web check\n> tsc --noEmit\n> eslint .\nChecking formatting…",
            },
          }
        : emptyStreaming;
  const gate = mode === "Approval needed" ? push.id : null;
  return (
    <>
      <p className="g-intro">
        Composed from the client’s transcript and toolbar components. Shortened
        worktree UI exchange; fixture data only. Try the controls, resize the
        page, or switch themes.
      </p>
      <div className="g-scene-modes" role="group" aria-label="Scene state">
        {modes.map((value) => (
          <button
            key={value}
            aria-pressed={mode === value}
            onClick={() => {
              setMode(value);
              setInspection(null);
              setDecision("");
              setSteer("");
            }}
          >
            {value}
          </button>
        ))}
      </div>
      {mode === "Answer needed" && (
        <div
          className="g-scene-modes"
          role="group"
          aria-label="Current question"
        >
          <span className="g-caption">Answering</span>
          <button
            aria-pressed={!answerSecond}
            onClick={() => setAnswerSecond(false)}
          >
            Question 1
          </button>
          <button
            aria-pressed={answerSecond}
            onClick={() => setAnswerSecond(true)}
          >
            Question 2
          </button>
        </div>
      )}
      <div className="g-scene">
        <div className="g-scene-head">
          <span>Worktree controls in the web UI</span>
          <span className="g-caption">deft-walnut · 147k / 272k</span>
        </div>
        <TranscriptContent
          nodes={buildTree(entries)}
          streaming={streaming}
          pending={[]}
          ask={
            mode === "Answer needed" ? (answerSecond ? secondAsk : ask) : null
          }
          gatedCall={gate}
          onGateSlot={setGateSlot}
          onRecall={noop}
          onInspect={setInspection}
          onOpenFile={(_, callId) => setInspection(callId)}
        />
        {gate &&
          gateSlot &&
          createPortal(
            <div>
              <GateControls
                busy={false}
                coldLine={null}
                steer={steer}
                setSteer={setSteer}
                approve={() =>
                  setDecision("Allowed locally — no command was run.")
                }
                abort={() =>
                  setDecision("Stopped locally — no session was changed.")
                }
                redirect={() =>
                  setDecision(
                    `Redirected locally: ${steer.trim()} — nothing was sent.`,
                  )
                }
              />
              {decision && (
                <p className="dim" role="status">
                  {decision}
                </p>
              )}
            </div>,
            gateSlot,
          )}
        {inspection && (
          <details className="g-inspection" open>
            <summary>Fixture call — raw output</summary>
            <pre>
              {inspection === push.id
                ? `${push.input.command}\n\nNo result: this fixture is waiting for approval.`
                : "ToolResult" in result.payload
                  ? result.payload.ToolResult.raw.content
                  : ""}
            </pre>
          </details>
        )}
        <ComposerFixture
          key={mode}
          running={running}
          turn={turnState(
            running,
            null,
            false,
            false,
            null,
            null,
            mode === "Answer needed" ? "ask" : gate ? "gate" : null,
          )}
        />
      </div>
    </>
  );
}

export function ComposerSpecimens() {
  return (
    <div className="g-composer-specimens">
      <Example title="Your turn · rest">
        <ComposerFixture turn={turnState(false, null)} />
      </Example>
      <Example title="Your turn · focused">
        <ComposerFixture turn={turnState(false, null)} focused />
      </Example>
      <Example title="Answer needed · multiline draft">
        <ComposerFixture
          running
          focused
          initial={
            "Keep it uncommitted for now.\n\nI’d like to compare the mobile layout before we publish it."
          }
          turn={turnState(true, null, false, false, null, null, "ask")}
        />
      </Example>
      <Example title="Approval needed">
        <ComposerFixture
          running
          turn={turnState(true, null, false, false, null, null, "gate")}
        />
      </Example>
    </div>
  );
}

function Example({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="g-example">
      <h3>{title}</h3>
      {children}
    </div>
  );
}

export function SyntaxSpecimens() {
  return (
    <div className="g-syntax">
      <Example title="Shell command · paths and arguments">
        <div className="transcript">
          <div className="call">
            <CallBlock call={push} />
          </div>
        </div>
      </Example>
      <Example title="File edit · added and deleted lines">
        <ResultBlock
          tool="edit"
          seq={0}
          raw={{
            is_error: false,
            content:
              'Applied replace — the file has been updated.\n\n===DIFF===\n@@ -1,2 +1,3 @@\n-<Settings draft={draft} />\n+<Settings draft={draft} onDraft={setDraft} />\n+<WorktreeNotice branch="efference/worktree-controls" />',
          }}
          wire="Raw"
        />
      </Example>
      <Example title="Failure · error and recovery">
        <ResultBlock
          tool="bash"
          seq={0}
          raw={{
            is_error: true,
            content:
              "fatal: the remote contains work that you do not have locally\nFetch the remote branch before trying again.",
          }}
          wire="Raw"
        />
      </Example>
    </div>
  );
}

// The fixtures are files under `public/gallery/`, referenced by path rather
// than imported: an import of a small SVG is inlined as a `data:` URI, which
// the client's CSP blocks (`default-src 'self'`, no `img-src`; src/daemon/tcp.rs).
// Each drawing is its own aspect ratio, so a glance at the thumbnail says what
// the lightbox has to fit.
const FIXTURE_IMAGES = [
  { src: "/gallery/wide.svg", width: 1600, height: 900 },
  { src: "/gallery/square.svg", width: 1000, height: 1000 },
  { src: "/gallery/tall.svg", width: 900, height: 1600 },
];

export function ImageSpecimens() {
  return (
    <div className="g-syntax">
      <Example title="Result images · wide, square, tall">
        <div className="transcript">
          <div className="block">
            {FIXTURE_IMAGES.map((image) => (
              <ZoomableImage
                key={image.width}
                src={image.src}
                width={image.width}
                height={image.height}
                label={`image/svg+xml ${image.width}×${image.height}`}
              />
            ))}
          </div>
        </div>
      </Example>
      <p className="g-caption">
        Tap any image to open the lightbox. Pinch or scroll to zoom, double-tap
        to toggle, drag to pan, and Escape, the ✕, or the backdrop to close.
      </p>
    </div>
  );
}
