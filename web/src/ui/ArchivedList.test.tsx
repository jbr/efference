// @vitest-environment jsdom
// A recent archived row (ui/ArchivedList.tsx) offers two verbs: the row resumes
// the session, and the menu beside it switches models — which resumes it on the
// chosen ones instead. The row is one button, so the menu is a sibling; this pins
// that neither hit area swallows the other.

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type { ArchivedSession } from "../api/http.ts";
import { ArchivedList } from "./ArchivedList.tsx";

const session = (id: string): ArchivedSession => ({
  id,
  cwd: "/Users/x/p",
  title: `${id} work`,
  last_active_at: 1_760_000_000_000,
  state: "archived",
});

let host: HTMLDivElement;
let root: Root;

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

async function render(sessions = [session("alpha")]) {
  const props = {
    sessions,
    remainderCount: 0,
    resuming: null,
    onResume: vi.fn(),
    onSwitch: vi.fn(),
    onOpenPicker: vi.fn(),
  };
  await act(async () => root.render(<ArchivedList {...props} />));
  return props;
}

const trigger = (id: string) =>
  host.querySelector<HTMLButtonElement>(
    `button[aria-label="actions for ${id}"]`,
  )!;

test("the row resumes the session it names", async () => {
  const props = await render();
  await act(async () =>
    host.querySelector<HTMLButtonElement>(".archived-row")!.click(),
  );
  expect(props.onResume).toHaveBeenCalledWith("alpha");
  expect(props.onSwitch).not.toHaveBeenCalled();
});

test("the menu beside the row switches models without resuming", async () => {
  const props = await render();
  await act(async () => trigger("alpha").click());
  const item = host.querySelector<HTMLButtonElement>('[role="menuitem"]')!;
  expect(item.textContent).toBe("Switch model…");
  await act(async () => item.click());

  expect(props.onSwitch).toHaveBeenCalledWith("alpha");
  expect(props.onResume).not.toHaveBeenCalled();
  expect(host.querySelector('[role="menu"]')).toBeNull();
});

test("each row carries its own menu", async () => {
  const props = await render([session("alpha"), session("beta")]);
  await act(async () => trigger("beta").click());
  await act(async () =>
    host.querySelector<HTMLButtonElement>('[role="menuitem"]')!.click(),
  );
  expect(props.onSwitch).toHaveBeenCalledWith("beta");
});

test("Escape closes the menu", async () => {
  await render();
  await act(async () => trigger("alpha").click());
  expect(host.querySelector('[role="menu"]')).not.toBeNull();
  await act(async () => {
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
  });
  expect(host.querySelector('[role="menu"]')).toBeNull();
});
