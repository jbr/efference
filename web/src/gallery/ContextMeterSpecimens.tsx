import { ContextMeter } from "../ui/TopBar.tsx";
import { Specimen, States } from "./Specimens.tsx";

export function ContextMeterSpecimens() {
  return (
    <>
      <p className="g-intro">
        Production context-meter interaction styles. Hover and press brighten
        the glow without changing occupancy or warning hue. Open is pinned as if
        its popup were visible; these specimens do not open popups or summarize
        history.
      </p>
      {[0, 10, 50, 75, 90, 100, null].map((percent) => (
        <Specimen
          key={percent ?? "unknown"}
          name={percent === null ? "Usage unknown" : `${percent}% used`}
        >
          <States
            states={["rest", "hover", "active", "open", "focus-visible"]}
            render={(cls) => (
              <button
                type="button"
                className={`context-popup-trigger ${cls}`}
                aria-label="Context details specimen"
                aria-expanded={cls === "is-open"}
              >
                <ContextMeter
                  context={{
                    used: percent === null ? null : percent * 2000,
                    limit: 200_000,
                  }}
                />
              </button>
            )}
          />
        </Specimen>
      ))}
    </>
  );
}
