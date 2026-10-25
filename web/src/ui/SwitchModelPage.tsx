// Switching a session's models (docs/resumption.md): a full page reached from the
// session's details, and from the overview's rows. A running session restarts by
// archiving and resuming; a dormant one resumes on the chosen models in one
// patch. The only thing that differs from a plain resume is the roles the resume
// names.
//
// The page is the launch form's settings surface less what a restart cannot move:
// effort and temperature come from the header the session was created with, and
// so does its location. Each role opens the same model picker, so browsing and
// the catalog behave as they do there.

import { useEffect, useRef, useState } from "react";
import { AppHeader } from "./AppHeader.tsx";
import { kilotokens } from "./compact.ts";
import { homeRelative } from "./identity.ts";
import { ModelPicker } from "./ModelPicker.tsx";
import { ModelControl, Setting } from "./NewSessionSettings.tsx";
import type { Role } from "./pickerRows.ts";
import {
  keeping,
  recordedFromSelections,
  selectionText,
  switchFields,
  triageSeparate,
  triageValue,
} from "./switchModel.ts";
import type { SwitchDraft, SwitchTarget } from "./switchModel.ts";
import {
  archiveSession,
  fetchDormantSession,
  resumeSession,
  type DormantSession,
} from "../api/http.ts";
import "./NewSessionForm.css";
import "./SwitchModelPage.css";

export function SwitchModelPage({
  target,
  onClose,
  onSwitched,
  onRestarting,
  initialDraft = keeping,
}: {
  target: SwitchTarget;
  onClose: () => void;
  /** The restart was asked for: the client takes over from here. */
  onSwitched: () => void;
  /** A restart is under way, and again when one fails: the view it opened over
   * is unreadable until the resume claims the session, and must say so rather
   * than treat the gap as a session that ended. */
  onRestarting?: (restarting: boolean) => void;
  /** The page opens with nothing changed; the gallery and the tests render it
   * with a choice already made. */
  initialDraft?: SwitchDraft;
}) {
  const { id, cwd, live, cold } = target;
  const [draft, setDraft] = useState<SwitchDraft>(initialDraft);
  const [picking, setPicking] = useState<Role | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const pending = useRef(false);

  // A dormant session's roles and context size live on its single-row read,
  // which the listing does not carry. A caller that already read them hands them
  // over on the target (the gallery, a test), and then nothing is fetched.
  const [dormant, setDormant] = useState<DormantSession | null>(null);
  const [dormantFailed, setDormantFailed] = useState(false);
  useEffect(() => {
    if (live || target.recorded !== null) return;
    let current = true;
    fetchDormantSession(id)
      .then((row) => {
        if (current) setDormant(row);
      })
      .catch(() => {
        if (current) setDormantFailed(true);
      });
    return () => {
      current = false;
    };
  }, [id, live, target.recorded]);

  const recorded =
    target.recorded ?? recordedFromSelections(dormant?.selections);
  const contextTokens = target.contextTokens ?? dormant?.context_tokens ?? null;
  const fields = switchFields(draft, recorded);
  const read =
    contextTokens === null
      ? "the context uncached"
      : `~${kilotokens(contextTokens)} tokens uncached`;

  function pick(role: Role, selection: { connection: string; model: string }) {
    setDraft((current) =>
      role === "triage"
        ? { ...current, triage: { kind: "separate", selection } }
        : { ...current, [role]: selection },
    );
  }

  async function restart() {
    if (fields === null || cwd === null || pending.current) return;
    pending.current = true;
    setBusy(true);
    setError(null);
    onRestarting?.(true);
    try {
      // Archive first: a resume is what reads a switch, and a running session is
      // not resumed. The host answers the archive once teardown has run, so the
      // resume that follows finds the session dormant.
      if (live) await archiveSession(id);
      await resumeSession(id, cwd, fields);
      onSwitched();
    } catch (failure) {
      setError(`Switching ${id}: ${String(failure)}`);
      onRestarting?.(false);
    } finally {
      pending.current = false;
      setBusy(false);
    }
  }

  const triage = triageValue(draft, recorded);
  const separate = triageSeparate(draft, recorded);
  // A running session knows its own roles once its view has them; a dormant one
  // knows them once its row is read. Until then the rows wait, and a list that
  // could not be read still offers them, on nothing chosen.
  const rows = live
    ? recorded !== null
    : target.recorded !== null || dormant !== null || dormantFailed;

  return (
    <div
      className="new-session-page switch-model-page"
      role="dialog"
      aria-label="Switch model"
    >
      <AppHeader
        className="new-session-bar"
        innerClassName="new-session-bar-inner"
        onHome={onClose}
      >
        <button onClick={onClose}>Cancel</button>
      </AppHeader>
      <main className="new-session" aria-label="Switch model">
        <header className="new-session-head">
          {picking !== null && (
            <button
              className="quiet new-session-back"
              onClick={() => setPicking(null)}
            >
              ← Back to models
            </button>
          )}
          <h1>
            {picking === null ? "Switch model" : `Choose ${picking} model`}
          </h1>
          {picking === null && (
            <p className="new-session-description">
              <span className="switch-model-id">{id}</span>
              {cwd !== null && <> in {homeRelative(cwd)}</>}
            </p>
          )}
        </header>
        <div className="new-session-body">
          {picking !== null && cwd !== null ? (
            <ModelPicker
              key={picking}
              role={picking}
              cwd={cwd}
              config={null}
              onPick={(selection) => pick(picking, selection)}
              onClose={() => setPicking(null)}
            />
          ) : !rows ? (
            <p className="dim" role="status">
              Reading the session's models…
            </p>
          ) : (
            <>
              <Setting
                label="Main model"
                reset={
                  draft.main === null
                    ? undefined
                    : () => setDraft({ ...draft, main: null })
                }
              >
                <ModelControl
                  label="Main model"
                  value={selectionText(draft.main ?? recorded?.main ?? null)}
                  onClick={() => setPicking("main")}
                />
              </Setting>
              <Setting
                label="Spotter model"
                reset={
                  draft.spotter === null
                    ? undefined
                    : () => setDraft({ ...draft, spotter: null })
                }
              >
                <ModelControl
                  label="Spotter model"
                  value={selectionText(
                    draft.spotter ?? recorded?.spotter ?? null,
                  )}
                  onClick={() => setPicking("spotter")}
                />
              </Setting>
              <Setting
                label="Triage model"
                action={
                  separate ? (
                    <button
                      className="quiet setting-reset"
                      onClick={() =>
                        setDraft({ ...draft, triage: { kind: "shared" } })
                      }
                    >
                      Use the main model
                    </button>
                  ) : undefined
                }
                reset={
                  draft.triage.kind === "keep"
                    ? undefined
                    : () => setDraft({ ...draft, triage: { kind: "keep" } })
                }
              >
                <ModelControl
                  label="Triage model"
                  value={triage}
                  onClick={() => setPicking("triage")}
                />
              </Setting>
            </>
          )}
        </div>
        {picking === null && (
          <footer className="new-session-foot">
            {error !== null && (
              <p className="new-session-issue" role="alert">
                {error}
              </p>
            )}
            <p className="switch-model-note">
              {live ? (
                <>
                  Switching restarts {id}: it archives the session — background
                  processes stop — and resumes it on the models you choose.
                </>
              ) : (
                <>Resuming {id} starts it on the models you choose.</>
              )}
            </p>
            <p className="switch-model-note">
              Its first send after that reads {read}.
              {cold === null
                ? ""
                : " Its cache is already believed expired, so nothing cached is lost."}{" "}
              Summarizing the history first reads less.
            </p>
            <button
              className="primary"
              disabled={fields === null || busy || cwd === null}
              onClick={() => void restart()}
            >
              {busy ? "Switching…" : "Switch model"}
            </button>
          </footer>
        )}
      </main>
    </div>
  );
}
