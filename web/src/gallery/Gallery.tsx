// The design gallery: the client's widgets on one page, on fixture data, for
// iterating on the visual language without driving a live session to each
// state. Dev builds only — main.tsx imports it behind `?gallery` under
// import.meta.env.DEV, so it is never in the shipped bundle.
//
// Components that render from props alone are the real ones; the stateful
// surfaces (the composer, the resume list, the picker shell) are their markup
// with the same class names, so the styles under test are the production
// stylesheet either way. Each interactive control also appears pinned in its
// hover and focus states (pseudoStates.ts).

import { type ReactNode, useEffect, useState } from "react";
import type { ProjectEntry, SessionPreview } from "../api/sessionForm.ts";
import { Markdown } from "../ui/Markdown.tsx";
import { LocationRow } from "../ui/LocationRow.tsx";
import { ModelPickerRow } from "../ui/ModelPickerRow.tsx";
import { Settings } from "../ui/NewSessionSettings.tsx";
import type { Row } from "../ui/pickerRows.ts";
import { SessionCard } from "../ui/SessionCard.tsx";
import { HoverCard, MoreButton, Tab } from "../ui/SessionSwitcher.tsx";
import type { CardModel } from "../ui/overview.ts";
import { emptyDraft, type Draft } from "../ui/sessionForm.ts";
import { setThemeChoice, themeChoice } from "../ui/theme.ts";
import { installPseudoStates } from "./pseudoStates.ts";
import "../ui/NewSessionForm.css";
import "../ui/ModelPicker.css";
import "../ui/ResumePage.css";
import "../ui/SessionList.css";
import "../ui/Transcript.css";
import "./Gallery.css";

// Fixture time: a model row computes its age fact against it.
const OPENED_AT = 1_760_000_000_000;

const STATES = ["rest", "hover", "focus-visible"] as const;

/** One control in each pinned state, side by side. `render` receives the
 * state class to put on the element the pseudo-class would match; when that
 * element is inside a component that takes no class, `on` names it by
 * selector and the class is set on it after render instead. */
function States({
  render,
  on,
  states = STATES,
}: {
  render: (cls: string) => ReactNode;
  on?: string;
  states?: readonly string[];
}) {
  return (
    <div className="g-states">
      {states.map((state) => {
        const cls = state === "rest" ? "" : `is-${state}`;
        return (
          <figure
            key={state}
            className="g-state"
            ref={(figure) => {
              if (on !== undefined && cls !== "") {
                figure?.querySelector(on)?.classList.add(cls);
              }
            }}
          >
            {render(on === undefined ? cls : "")}
            <figcaption>{state}</figcaption>
          </figure>
        );
      })}
    </div>
  );
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="g-section">
      <h2 className="g-title">{title}</h2>
      {children}
    </section>
  );
}

function Specimen({ name, children }: { name: string; children: ReactNode }) {
  return (
    <div className="g-specimen">
      <div className="g-name">{name}</div>
      <div className="g-body">{children}</div>
    </div>
  );
}

const projects: ProjectEntry[] = [
  "efference",
  "semantic-edit-mcp",
  "trillium",
].map((name, index) => ({
  label: name,
  cwd: `/Users/jbr/code/${name}`,
  config: null,
  config_key: "",
  last_used_at: 1_760_000_000 - index * 3600,
}));

const preview: SessionPreview = {
  effective: {
    main: {
      value: {
        connection: "anthropic",
        provider: "anthropic",
        model: "claude-sonnet-5-5",
      },
      source: "user_default",
    },
    spotter: {
      value: {
        connection: "typesafe",
        provider: "openrouter",
        model: "jev-latest",
      },
      source: "user_default",
    },
    triage: { value: { mode: "shared" }, source: "shared_main" },
    effort: { value: null, source: "provider_default" },
    temperature: { value: null, source: "provider_default" },
  },
  issues: [],
};

const pickerRows: Row[] = [
  {
    kind: "model",
    node: {
      kind: "model",
      connection: "anthropic",
      model: "claude-sonnet-5-5",
      name: "Claude Sonnet 5.5",
      recent: 1,
    },
    model: { model: "claude-sonnet-5-5", name: "Claude Sonnet 5.5" },
    recent: true,
  },
  {
    kind: "model",
    node: { kind: "model", connection: "meta", model: "muse-spark-1.3" },
    model: { model: "muse-spark-1.3" },
    recent: true,
  },
  {
    kind: "connection",
    node: { kind: "connection", connection: "anthropic" },
    entry: {
      connection: "anthropic",
      provider: "anthropic",
      status: "fresh",
      models: Array.from({ length: 14 }, (_, i) => ({ model: `m${i}` })),
    },
  },
  {
    kind: "connection",
    node: { kind: "connection", connection: "codex" },
    entry: {
      connection: "codex",
      provider: "openai",
      status: "fresh",
      models: Array.from({ length: 7 }, (_, i) => ({ model: `m${i}` })),
    },
  },
];

const card = (over: Partial<CardModel>): CardModel => ({
  id: "quick-sparrow",
  title: "Implementing worktree UI in the TUI",
  cwd: "/Users/jbr/code/efference",
  status: "idle",
  attention: "your_turn",
  processes: 0,
  supervisor: null,
  supervises: [],
  activity: {
    kind: "reply",
    detail: "Landed. It wasn't a fast-forward as it stood, so I rebased first:",
  },
  park: null,
  context: { used: 233_000, limit: 1_000_000 },
  costUsd: null,
  models: { main: "deepseek-flash", spotter: "jev-latest", triage: null },
  ...over,
});

const tabCards: CardModel[] = [
  card({
    id: "still-kestrel",
    title: "Smoke-testing worktrees",
    status: "gated",
    attention: "blocked",
    activity: { kind: "gate", tool: "bash", detail: "git push origin main" },
  }),
  card({ id: "quick-sparrow", title: "Worktree UI in the TUI" }),
  card({
    id: "fold-probe",
    title: "Probing context folding",
    status: "stalled",
    attention: "blocked",
    activity: { kind: "stall", detail: "429 from the provider" },
  }),
  card({
    id: "cool-harbor",
    title: "Cold parks",
    status: "parked",
    attention: "blocked",
    park: "expired",
  }),
  card({
    id: "deft-walnut",
    title: "Web worktree controls",
    status: "running",
    attention: "working",
  }),
  card({ id: "keen-lynx", title: null, status: "idle", attention: "new" }),
];

const archived = [
  ["Implementing worktree UI in the TUI", "quick-sparrow", "48 minutes ago"],
  [null, "lucid-plover", "yesterday"],
  ["Probing context folding behavior", "fold-probe", "2 days ago"],
] as const;

export function Gallery() {
  const [theme, setTheme] = useState(themeChoice());
  const [draft, setDraft] = useState<Draft>(() => emptyDraft(projects[0].cwd));
  useEffect(installPseudoStates, []);

  return (
    <div className="gallery">
      <header className="g-head">
        <h1>efference gallery</h1>
        <span className="spacer" />
        {(["after-dark", "lilac", "system"] as const).map((choice) => (
          <button
            key={choice}
            aria-pressed={theme === choice}
            className={theme === choice ? "primary" : undefined}
            onClick={() => {
              setThemeChoice(choice);
              setTheme(choice);
            }}
          >
            {choice}
          </button>
        ))}
      </header>

      <Section title="Buttons">
        <Specimen name="primary — button.primary">
          <States
            render={(cls) => (
              <button className={`primary ${cls}`}>Start session</button>
            )}
          />
        </Specimen>
        <Specimen name="default (unnamed) — button">
          <States render={(cls) => <button className={cls}>Resume…</button>} />
        </Specimen>
        <Specimen name="disabled">
          <div className="g-states">
            <button className="primary" disabled>
              Start session
            </button>
            <button disabled>Resume…</button>
          </div>
        </Specimen>
        <Specimen name="toolbar — .attach-btn / .interrupt-btn / .resume-btn">
          <div className="composer-toolbar">
            <States
              render={(cls) => (
                <button type="button" className={`attach-btn ${cls}`}>
                  <PaperClip />
                  <span>attach</span>
                </button>
              )}
            />
            <States
              render={(cls) => (
                <button type="button" className={`interrupt-btn ${cls}`}>
                  interrupt
                </button>
              )}
            />
            <States
              render={(cls) => (
                <button type="button" className={`resume-btn ${cls}`}>
                  resume
                </button>
              )}
            />
          </div>
        </Specimen>
        <Specimen name="pill — .t-fold">
          <div className="transcript">
            <States
              states={["rest", "hover"]}
              render={(cls) => (
                <button className={`t-fold ${cls}`}>Thought · 541 words</button>
              )}
            />
          </div>
        </Specimen>
        <Specimen name="quiet — button.quiet (.effort-default, .setting-reset, .picker-root, .new-session-back)">
          <div className="new-session g-inline">
            <States
              render={(cls) => (
                <button className={`quiet effort-default ${cls}`}>
                  Use default
                </button>
              )}
            />
          </div>
        </Specimen>
        <Specimen name="text field — input">
          <States
            states={["rest", "focus-visible"]}
            render={(cls) => (
              <input className={cls} placeholder="Filter by title or id" />
            )}
          />
        </Specimen>
        <Specimen name="selection — ::selection (--selection)">
          <p className="g-prose">
            Landed. It wasn&apos;t a{" "}
            <mark className="g-selection">fast-forward as it stood, so I</mark>{" "}
            rebased first.
          </p>
        </Specimen>
      </Section>

      <Section title="Neon">
        <Specimen name="glyphs — .prompt / .note-glyph / .consolidation-rule .glyph">
          <div className="g-row g-mono">
            <span className="prompt">›</span>
            <span className="note-glyph">✦</span>
            <span className="consolidation-rule">
              <span className="glyph">◆</span>
            </span>
          </div>
        </Specimen>
      </Section>

      <Section title="Session tabs">
        <Specimen name="one per dot — needs, turn, stalled, parked, working, idle; +N with a waiting session">
          <nav className="switcher g-switcher">
            {tabCards.map((c) => (
              <span key={c.id} className="switcher-slot">
                <Tab card={c} />
              </span>
            ))}
            <MoreButton total={3} waiting={1} onClick={() => {}} />
          </nav>
        </Specimen>
        <Specimen name="tab states — .switcher-tab">
          <States
            on=".switcher-tab"
            render={() => (
              <nav className="switcher g-switcher">
                <span className="switcher-slot">
                  <Tab card={tabCards[1]} />
                </span>
              </nav>
            )}
          />
        </Specimen>
        <Specimen name="hover card — .switcher-pop">
          <div className="switcher g-switcher g-pop-host">
            <span className="switcher-slot">
              <HoverCard card={tabCards[0]} />
            </span>
          </div>
        </Specimen>
      </Section>

      <Section title="Composer">
        <div className="g-composer composer">
          <div className="composer-inner">
            <div className="composer-input">
              <div className="composer-text-row">
                <textarea placeholder="send a turn…" rows={3} />
              </div>
              <div className="composer-toolbar">
                <div className="composer-toolbar-left">
                  <button type="button" className="attach-btn">
                    <PaperClip />
                    <span>attach</span>
                  </button>
                  <span className="turn-state your-turn">your turn</span>
                </div>
                <div className="composer-toolbar-right">
                  <span className="composer-hint">shift-enter for newline</span>
                  <button className="send-btn">
                    <span>send</span>
                    <kbd className="send-kbd">↵</kbd>
                  </button>
                </div>
              </div>
            </div>
          </div>
        </div>
        <Specimen name="focused — .composer-input:focus-within">
          <div className="g-composer composer">
            <div className="composer-inner">
              <div className="composer-input is-focus-within">
                <div className="composer-text-row">
                  <textarea placeholder="send a turn…" rows={2} />
                </div>
                <div className="composer-toolbar">
                  <div className="composer-toolbar-left">
                    <span className="turn-state dim">working…</span>
                  </div>
                  <div className="composer-toolbar-right">
                    <button type="button" className="interrupt-btn">
                      interrupt
                    </button>
                    <button className="send-btn">
                      <span>send</span>
                      <kbd className="send-kbd">↵</kbd>
                    </button>
                  </div>
                </div>
              </div>
            </div>
          </div>
        </Specimen>
      </Section>

      <Section title="Transcript">
        <div className="transcript g-transcript">
          <div className="entry user">
            <div className="label">you</div>
            <div className="content">
              <Markdown>
                {
                  "I think this should be fastforward on main, right? Worth checking?"
                }
              </Markdown>
            </div>
          </div>
          <div className="entry">
            <div className="label" />
            <div className="content">
              <button className="t-fold">Thought · 541 words</button>
            </div>
          </div>
          <div className="entry">
            <div className="label" />
            <div className="content g-mono">
              <span className="call-sig shell-command">
                git log --oneline -3
              </span>
            </div>
          </div>
          <div className="entry">
            <div className="label" />
            <div className="content">
              <div className="note">
                <span className="note-glyph">✧</span>
                <span className="note-text">A note in the flow.</span>
              </div>
            </div>
          </div>
        </div>
      </Section>

      <Section title="Session card">
        <div className="overview g-overview">
          <div className="card-grid">
            <SessionCard
              card={card({})}
              warning={null}
              archiving={false}
              onArchive={() => {}}
            />
            <SessionCard
              card={card({
                id: "still-kestrel",
                title: "Smoke-testing worktree session behavior",
                status: "gated",
                attention: "blocked",
                activity: {
                  kind: "gate",
                  tool: "bash",
                  detail: "git push origin main",
                },
              })}
              warning={null}
              archiving={false}
              onArchive={() => {}}
            />
            <SessionCard
              card={card({
                id: "deft-walnut",
                title: null,
                status: "running",
                attention: "working",
                activity: {
                  kind: "intent",
                  detail: "Reading the composer CSS",
                },
                processes: 2,
              })}
              warning={null}
              archiving={false}
              onArchive={() => {}}
            />
          </div>
        </div>
      </Section>

      <Section title="Forms — session settings">
        <div className="new-session-page g-page">
          <main className="new-session g-form">
            <div className="new-session-body">
              <Settings
                draft={draft}
                preview={preview}
                onDraft={setDraft}
                onPick={() => {}}
              />
            </div>
            <footer className="new-session-foot">
              <button className="primary">Start session</button>
            </footer>
          </main>
        </div>
      </Section>

      <Section title="Forms — model picker">
        <div className="new-session-page g-page">
          <main className="new-session g-form">
            <div className="new-session-body">
              <div className="picker">
                <nav className="picker-path">
                  <button className="picker-root" disabled>
                    All models
                  </button>
                  <span aria-hidden="true"> / </span>
                </nav>
                <div className="picker-input">
                  <input placeholder="Search models" />
                </div>
                <div className="picker-rows">
                  <h2 className="picker-section">Recent models</h2>
                  {pickerRows.slice(0, 2).map((row, i) => (
                    <ModelPickerRow
                      key={i}
                      row={row}
                      chips={[]}
                      openedAt={OPENED_AT}
                      selected={false}
                      onPick={() => {}}
                    />
                  ))}
                  <h2 className="picker-section">Browse connections</h2>
                  {pickerRows.slice(2).map((row, i) => (
                    <ModelPickerRow
                      key={i}
                      row={row}
                      chips={[]}
                      openedAt={OPENED_AT}
                      selected={i === 1}
                      onPick={() => {}}
                    />
                  ))}
                </div>
              </div>
            </div>
          </main>
        </div>
      </Section>

      <Section title="Forms — resume">
        <div className="resume-page g-resume">
          <div className="resume-page-body">
            <div className="resume-locations-list">
              {projects.map((entry) => (
                <LocationRow
                  key={entry.cwd}
                  entry={entry}
                  projects={projects}
                  selected={false}
                  editing={false}
                  onSelect={() => {}}
                />
              ))}
            </div>
            <div className="resume-search-bar">
              <input
                className="resume-search-input"
                placeholder="Filter by title or id"
              />
            </div>
            <div className="resume-results-list">
              {archived.map(([title, id, age], i) => (
                <div
                  key={id}
                  className={`resume-result-row${i === 1 ? " selected" : ""}`}
                >
                  <span className="resume-result-title">
                    {title ?? <span className="dim">Untitled</span>}
                  </span>
                  <span className="resume-result-id">{id}</span>
                  <span className="resume-result-age">{age}</span>
                  <span className="resume-hint">resume ›</span>
                </div>
              ))}
            </div>
          </div>
        </div>
      </Section>
    </div>
  );
}

function PaperClip() {
  return (
    <svg
      width="16"
      height="16"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="m21.44 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l8.57-8.57A4 4 0 1 1 18 8.84l-8.59 8.57a2 2 0 0 1-2.83-2.83l8.49-8.48" />
    </svg>
  );
}
