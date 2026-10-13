import type { CSSProperties } from "react";
import { PaperClip } from "../ui/ComposerToolbar.tsx";
import { Specimen, States } from "./Specimens.tsx";

export function ButtonSpecimens() {
  return (
    <>
      <Specimen name="primary — button.primary">
        <States
          render={(cls) => (
            <button className={`primary ${cls}`}>Start session</button>
          )}
        />
      </Specimen>
      <Specimen name="pressed interior — centered / even hue spill">
        <div className="g-states">
          <figure className="g-state">
            <button className="primary is-active">Start session</button>
            <figcaption>centered spill</figcaption>
          </figure>
          <figure className="g-state">
            <button
              className="primary is-active"
              style={
                {
                  "--pressed-image":
                    "linear-gradient(var(--press-color), var(--press-color))",
                } as CSSProperties
              }
            >
              Start session
            </button>
            <figcaption>even spill</figcaption>
          </figure>
        </div>
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
            states={["rest", "hover", "active"]}
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
      <Specimen name="checkbox — unchecked / checked">
        <div className="new-session g-inline">
          {[false, true].map((checked) => (
            <div key={String(checked)} className="g-checkboxes">
              <p className="g-caption">{checked ? "Checked" : "Unchecked"}</p>
              <States
                render={(cls) => (
                  <label
                    className={`worktree-toggle ${cls === "is-hover" || cls === "is-active" ? cls : cls === "is-focus-visible" ? "is-focus-within" : ""}`}
                  >
                    <input
                      type="checkbox"
                      defaultChecked={checked}
                      className={cls}
                    />
                    Own worktree
                  </label>
                )}
              />
            </div>
          ))}
        </div>
      </Specimen>
      <Specimen name="selection — ::selection (--selection)">
        <p className="g-prose">
          Landed. It wasn&apos;t a{" "}
          <mark className="g-selection">fast-forward as it stood, so I</mark>{" "}
          rebased first.
        </p>
      </Specimen>
    </>
  );
}
