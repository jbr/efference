// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, test, vi } from "vitest";
import { SelectionControl } from "./SelectionControl.tsx";

test("selection fields retain their label, value and secondary configuration", () => {
  const markup = renderToStaticMarkup(
    <SelectionControl label="Choose location: /work/project" onClick={() => {}}>
      ~/project<code>alternate.toml</code>
    </SelectionControl>,
  );
  expect(markup).toContain('aria-label="Choose location: /work/project"');
  expect(markup).toContain("~/project");
  expect(markup).toContain("<code>alternate.toml</code>");
  expect(markup).toContain('aria-hidden="true"');
});

test("selection opens its chooser rather than submitting a containing form", async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const onClick = vi.fn();
  const onSubmit = vi.fn((event) => event.preventDefault());
  try {
    await act(async () =>
      root.render(
        <form onSubmit={onSubmit}>
          <SelectionControl label="Choose main model" onClick={onClick}>
            connection/model
          </SelectionControl>
        </form>,
      ),
    );
    await act(async () => host.querySelector("button")!.click());
    expect(onClick).toHaveBeenCalledOnce();
    expect(onSubmit).not.toHaveBeenCalled();
  } finally {
    await act(async () => root.unmount());
    host.remove();
  }
});
