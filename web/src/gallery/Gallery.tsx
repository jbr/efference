import { RangeContentSpecimens } from "./RangeContentSpecimens.tsx";
// The design gallery: the client's widgets on one page, on fixture data, for
// iterating on the visual language without driving a live session to each
// state. Dev builds only — main.tsx imports it behind `?gallery` under
// import.meta.env.DEV, so it is never in the shipped bundle.
//
// Components that render from props alone are the real ones; the stateful
// shells (the composer input, the resume list, the picker) retain production
// class names; transcript content and the composer toolbar are shared with the app.
// The styles under test are the production
// stylesheet either way. Each interactive control also appears pinned in its
// hover and focus states (pseudoStates.ts).

import { type ReactNode, useEffect, useState } from "react";
import { LightboxHost } from "../ui/Lightbox.tsx";
import { LocationRow } from "../ui/LocationRow.tsx";
import { ModelPickerRow } from "../ui/ModelPickerRow.tsx";
import { Settings } from "../ui/NewSessionSettings.tsx";
import { SessionCard } from "../ui/SessionCard.tsx";
import { HoverCard, MoreButton, Tab } from "../ui/SessionSwitcher.tsx";
import { emptyDraft, type Draft } from "../ui/sessionForm.ts";
import { setThemeChoice, themeChoice } from "../ui/theme.ts";
import { ContextMeter } from "../ui/TopBar.tsx";
import {
  GalleryScenes,
  ComposerSpecimens,
  ImageSpecimens,
  SyntaxSpecimens,
} from "./Scenes.tsx";
import {
  projects,
  preview,
  pickerRows,
  card,
  tabCards,
  archived,
} from "./fixtures.ts";
import { ButtonSpecimens } from "./ButtonSpecimens.tsx";
import { AmbientSpecimens } from "./AmbientSpecimens.tsx";
import { ContextMeterSpecimens } from "./ContextMeterSpecimens.tsx";

import { Specimen, States } from "./Specimens.tsx";
import { PaletteSpecimens } from "./PaletteSpecimens.tsx";
import { installPseudoStates } from "./pseudoStates.ts";
import "../ui/NewSessionForm.css";
import "../ui/ModelPicker.css";
import "../ui/ResumePage.css";
import "../ui/SessionList.css";
import "../ui/Transcript.css";
import "./Gallery.css";

// Fixture time: a model row computes its age fact against it.
const OPENED_AT = 1_760_000_000_000;

const sectionId = (title: string) =>
  title.toLowerCase().replace(/[^a-z0-9]+/g, "-");

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="g-section" id={sectionId(title)}>
      <h2 className="g-title">{title}</h2>
      {children}
    </section>
  );
}

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
            className="g-theme"
            onClick={() => {
              setThemeChoice(choice);
              setTheme(choice);
            }}
          >
            {choice}
          </button>
        ))}
      </header>

      <nav className="g-index" aria-label="Gallery sections">
        {[
          "Scenes",
          "Buttons",
          "Neon",
          "Session tabs",
          "Context window",
          "Context interactions",
          "Composer",
          "Code and results",
          "Expandable ranges",
          "Ambients",
          "Images",
          "Syntax palette",
          "Session card",
          "Forms — session settings",
          "Forms — model picker",
          "Forms — resume",
        ].map((title) => (
          <a key={title} href={`#${sectionId(title)}`}>
            {title.replace("Forms — ", "")}
          </a>
        ))}
      </nav>
      <Section title="Scenes">
        <GalleryScenes />
      </Section>
      <Section title="Buttons">
        <ButtonSpecimens />
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

      <Section title="Context window">
        <p className="g-intro">
          The fill warms after halfway and moves toward the warning hue near
          capacity. This is a visual occupancy cue, not a prediction of when the
          harness will consolidate.
        </p>
        <div className="g-contexts">
          {[10, 50, 75, 90, 100].map((percent) => (
            <figure className="g-state" key={percent}>
              <ContextMeter
                context={{ used: percent * 2000, limit: 200_000 }}
              />
              <figcaption>{percent}% used</figcaption>
            </figure>
          ))}
          <figure className="g-state">
            <ContextMeter context={{ used: null, limit: 200_000 }} />
            <figcaption>usage unknown</figcaption>
          </figure>
          <figure className="g-state">
            <ContextMeter context={{ used: 50_000, limit: null }} />
            <figcaption>window unknown</figcaption>
          </figure>
        </div>
      </Section>
      <Section title="Context interactions">
        <ContextMeterSpecimens />
      </Section>
      <Section title="Composer">
        <ComposerSpecimens />
      </Section>

      <Section title="Code and results">
        <SyntaxSpecimens />
      </Section>
      <Section title="Expandable ranges">
        <RangeContentSpecimens />
      </Section>
      <Section title="Ambients">
        <AmbientSpecimens />
      </Section>
      <Section title="Images">
        <ImageSpecimens />
      </Section>
      <Section title="Syntax palette">
        <PaletteSpecimens />
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
        <p className="g-caption">
          Main model: pinned hover. Other controls remain interactive.
        </p>
        <div className="new-session-page g-page">
          <main
            className="new-session g-form"
            ref={(el) =>
              el?.querySelector(".setting-pick")?.classList.add("is-hover")
            }
          >
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
        <p className="g-caption">
          First recent model: pinned hover. Codex: selected.
        </p>
        <div className="new-session-page g-page">
          <main className="new-session g-form">
            <div className="new-session-body">
              <div className="picker selection-panel">
                <nav className="picker-path">
                  <button className="picker-root" disabled>
                    All models
                  </button>
                  <span aria-hidden="true"> / </span>
                </nav>
                <div className="picker-input">
                  <input placeholder="Search models" />
                </div>
                <div
                  className="picker-rows selection-list"
                  ref={(el) =>
                    el?.querySelector(".picker-row")?.classList.add("is-hover")
                  }
                >
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
        <p className="g-caption">
          First location and result: pinned hover. Second result: selected.
        </p>
        <div className="resume-page g-resume">
          <div className="resume-page-body">
            <div
              className="resume-locations-list selection-list selection-panel"
              ref={(el) =>
                el?.querySelector(".location-pick")?.classList.add("is-hover")
              }
            >
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
            <div className="resume-results-list selection-list selection-panel">
              {archived.map(([title, id, age], i) => (
                <div
                  key={id}
                  className={`resume-result-row selection-row${i === 1 ? " selected" : i === 0 ? " is-hover" : ""}`}
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
      {/* The gallery mounts the app's one lightbox so the image specimens
          actually open it. */}
      <LightboxHost />
    </div>
  );
}
