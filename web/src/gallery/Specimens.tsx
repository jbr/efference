import type { ReactNode } from "react";

const STATES = ["rest", "hover", "active", "focus-visible"] as const;

/** One control in each pinned state, side by side. `render` receives the
 * state class to put on the element the pseudo-class would match; when that
 * element is inside a component that takes no class, `on` names it by
 * selector and the class is set on it after render instead. */
export function States({
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

export function Specimen({
  name,
  children,
}: {
  name: string;
  children: ReactNode;
}) {
  return (
    <div className="g-specimen">
      <div className="g-name">{name}</div>
      <div className="g-body">{children}</div>
    </div>
  );
}
