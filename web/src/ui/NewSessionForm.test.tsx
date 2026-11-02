// @vitest-environment jsdom
import { act, type ComponentProps } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import {
  createSession,
  fetchProjects,
  forgetProject,
  previewSession,
} from "../api/sessionForm.ts";
import type { ProjectEntry, ResolvedOptions } from "../api/types.ts";
import { NewSessionForm } from "./NewSessionForm.tsx";
import { emptyDraft } from "./sessionForm.ts";

vi.mock("../api/sessionForm.ts", () => ({
  fetchProjects: vi.fn(),
  forgetProject: vi.fn(),
  previewSession: vi.fn(() => new Promise(() => {})),
  createSession: vi.fn(),
}));

const entry = (cwd: string): ProjectEntry => ({
  cwd,
  config: null,
  config_key: "builtin-default",
  label: "",
  last_used_at: 1,
});
const alpha = entry("/work/alpha");
const beta = entry("/work/beta");
let host: HTMLDivElement;
let root: Root;

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  vi.clearAllMocks();
  vi.mocked(fetchProjects).mockResolvedValue({ projects: [alpha, beta] });
  vi.mocked(forgetProject).mockResolvedValue({
    forgotten: { cwd: alpha.cwd, config_key: alpha.config_key },
  });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});

afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});

async function render(
  over: Partial<ComponentProps<typeof NewSessionForm>> = {},
) {
  const props = {
    draft: null,
    requested: null,
    page: "location" as const,
    onDraft: vi.fn(),
    onNavigate: vi.fn(),
    onBack: vi.fn(),
    onClose: vi.fn(),
    ...over,
  };
  await act(async () => root.render(<NewSessionForm {...props} />));
  return props;
}
const button = (label: string) =>
  host.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`)!;
async function click(element: HTMLElement) {
  await act(async () => element.click());
}

test("Edit adds leading removal controls while row selection stays active and exits editing", async () => {
  const props = await render();
  expect(host.querySelectorAll(".location-forget:not(:disabled)")).toHaveLength(
    0,
  );
  const hiddenRemove =
    host.querySelector<HTMLButtonElement>(".location-forget")!;
  expect(hiddenRemove.disabled).toBe(true);
  expect(hiddenRemove.tabIndex).toBe(-1);
  expect(hiddenRemove.parentElement?.getAttribute("aria-hidden")).toBe("true");
  expect(hiddenRemove.parentElement?.hasAttribute("inert")).toBe(true);
  await click(hiddenRemove);
  expect(forgetProject).not.toHaveBeenCalled();
  await click(button("Edit recent locations"));
  const row = host.querySelector(".location")!;
  expect(row.firstElementChild?.className).toBe("location-forget-slot");
  expect(host.querySelectorAll(".location-forget:not(:disabled)")).toHaveLength(
    2,
  );
  await click(row.querySelector<HTMLElement>(".location-pick")!);
  expect(props.onNavigate).toHaveBeenCalledWith("settings");
  expect(props.onDraft).toHaveBeenCalledWith(emptyDraft(alpha.cwd));
  expect(host.querySelectorAll(".location-forget:not(:disabled)")).toHaveLength(
    0,
  );
});

test("Done hides controls and Escape exits editing before cancelling the flow", async () => {
  const props = await render();
  await click(button("Edit recent locations"));
  await click(button("Done editing recent locations"));
  expect(host.querySelectorAll(".location-forget:not(:disabled)")).toHaveLength(
    0,
  );
  await click(button("Edit recent locations"));
  await act(async () => {
    document.dispatchEvent(
      new KeyboardEvent("keydown", {
        key: "Escape",
        bubbles: true,
        cancelable: true,
      }),
    );
  });
  expect(props.onClose).not.toHaveBeenCalled();
  expect(host.querySelectorAll(".location-forget:not(:disabled)")).toHaveLength(
    0,
  );
  await act(async () => {
    document.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
    );
  });
  expect(props.onClose).toHaveBeenCalledOnce();
});

test("forgetting the selected recent keeps the draft separate as Current location", async () => {
  const draft = { ...emptyDraft(alpha.cwd), temperature: 42 };
  const props = await render({ draft });
  await click(button("Edit recent locations"));
  await click(
    host.querySelector<HTMLElement>(".location-forget:not(:disabled)")!,
  );
  expect(forgetProject).toHaveBeenCalledWith(alpha);
  expect(props.onDraft).not.toHaveBeenCalled();
  expect(props.onNavigate).not.toHaveBeenCalled();
  expect(
    host.querySelector('section[aria-label="Recent locations"]')?.textContent,
  ).not.toContain("alpha");
  expect(
    host.querySelector('section[aria-label="Current location"]')?.textContent,
  ).toContain("alpha");
  expect(
    host.querySelector(".new-session-current .location-forget"),
  ).toBeNull();
  await click(
    host.querySelector<HTMLElement>(".new-session-current .location-pick")!,
  );
  expect(props.onDraft).toHaveBeenCalledWith(draft);
});

test("overlapping removals do not resurrect entries when responses arrive out of order", async () => {
  let resolveAlpha!: () => void;
  let resolveBeta!: () => void;
  const first = new Promise<Awaited<ReturnType<typeof forgetProject>>>(
    (resolve) => {
      resolveAlpha = () =>
        resolve({
          forgotten: { cwd: alpha.cwd, config_key: alpha.config_key },
        });
    },
  );
  const second = new Promise<Awaited<ReturnType<typeof forgetProject>>>(
    (resolve) => {
      resolveBeta = () =>
        resolve({ forgotten: { cwd: beta.cwd, config_key: beta.config_key } });
    },
  );
  vi.mocked(forgetProject)
    .mockReturnValueOnce(first)
    .mockReturnValueOnce(second);
  await render();
  await click(button("Edit recent locations"));
  const removes = host.querySelectorAll<HTMLButtonElement>(
    ".location-forget:not(:disabled)",
  );
  await act(async () => {
    removes[0].click();
    removes[1].click();
  });
  await act(async () => resolveBeta());
  expect(host.querySelectorAll(".location")).toHaveLength(1);
  await act(async () => resolveAlpha());
  expect(host.querySelectorAll(".location")).toHaveLength(0);
  expect(host.textContent).toContain("No recent locations yet.");
});

test("a direct model route without a draft cancels from its visible location surface", async () => {
  const props = await render({ page: "main" });
  await act(async () => {
    document.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
    );
  });
  expect(props.onClose).toHaveBeenCalledOnce();
  expect(props.onBack).not.toHaveBeenCalled();
});

test("Somewhere else remains a full-row disclosure and Escape closes it with focus restored", async () => {
  const props = await render();
  const trigger = host.querySelector<HTMLButtonElement>(
    ".directory-disclosure",
  )!;
  expect(trigger.getAttribute("aria-expanded")).toBe("false");
  await click(trigger);
  expect(host.querySelector(".directory-disclosure")).toBe(trigger);
  expect(trigger.getAttribute("aria-expanded")).toBe("true");
  expect(document.activeElement).toBe(host.querySelector("#launch-directory"));
  expect(props.onNavigate).not.toHaveBeenCalled();
  await act(async () => {
    document.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
    );
  });
  expect(host.querySelector("#launch-directory-form")).toBeNull();
  expect(trigger.getAttribute("aria-expanded")).toBe("false");
  expect(document.activeElement).toBe(trigger);
  expect(props.onClose).not.toHaveBeenCalled();
});

test("a typed directory commits when absolute or starting with ~ and not otherwise", async () => {
  const props = await render();
  await click(host.querySelector<HTMLElement>(".directory-disclosure")!);
  const submit = async (typed: string) => {
    const input = host.querySelector<HTMLInputElement>("#launch-directory")!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(
        HTMLInputElement.prototype,
        "value",
      )!.set!.call(input, typed);
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await click(
      host.querySelector<HTMLElement>(".new-session-other .primary")!,
    );
  };
  for (const typed of ["project", "~other/project"]) {
    await submit(typed);
    expect(props.onDraft, typed).not.toHaveBeenCalled();
    expect(host.textContent).toContain("starting with ~");
  }
  await submit("~/project");
  expect(props.onDraft).toHaveBeenCalledWith(emptyDraft("~/project"));
  expect(props.onNavigate).toHaveBeenCalledWith("settings");
});

test("the worktree toggle says what it does and keeps it in the draft", async () => {
  vi.mocked(previewSession).mockResolvedValue(previewAnswer());
  const draft = emptyDraft(alpha.cwd);
  const props = await render({ draft, page: "settings" });
  await act(async () => {});
  const box = host.querySelector<HTMLInputElement>(".worktree-toggle input")!;
  expect(box.checked).toBe(false);
  expect(host.textContent).toContain(".efference/worktrees/");
  await click(box);
  expect(props.onDraft).toHaveBeenCalledWith({ ...draft, worktree: true });
});

test("untoggling the worktree returns the draft to shared checkout", async () => {
  vi.mocked(previewSession).mockResolvedValue(previewAnswer());
  const draft = { ...emptyDraft(alpha.cwd), worktree: true };
  const props = await render({ draft, page: "settings" });
  await act(async () => {});
  const box = host.querySelector<HTMLInputElement>(".worktree-toggle input")!;
  expect(box.checked).toBe(true);
  await click(box);
  expect(props.onDraft).toHaveBeenCalledWith({ ...draft, worktree: false });
});

test("starting with the worktree on sends it in the creation body", async () => {
  Object.defineProperty(window, "scrollTo", {
    value: () => {},
    configurable: true,
  });
  vi.mocked(previewSession).mockResolvedValue(previewAnswer());
  vi.mocked(createSession).mockResolvedValue({ id: "quiet-fox" });
  const draft = { ...emptyDraft(alpha.cwd), worktree: true };
  await render({ draft, page: "settings" });
  await act(async () => {});
  await click(host.querySelector<HTMLElement>(".new-session-foot .primary")!);
  expect(createSession).toHaveBeenCalledWith({
    cwd: alpha.cwd,
    worktree: true,
  });
});

test("the worktree row is absent where the preview says a worktree cannot start", async () => {
  vi.mocked(previewSession).mockResolvedValue(previewAnswer(false));
  await render({ draft: emptyDraft(alpha.cwd), page: "settings" });
  await act(async () => {});
  expect(host.querySelector(".worktree-toggle")).toBeNull();
  expect(host.textContent).not.toContain("Worktree");
  expect(host.textContent).toContain("Spotter model");
});

test("a draft's worktree is cleared once the preview says it cannot start", async () => {
  vi.mocked(previewSession).mockResolvedValue(previewAnswer(false));
  const draft = { ...emptyDraft(alpha.cwd), worktree: true };
  const props = await render({ draft, page: "settings" });
  await act(async () => {});
  expect(props.onDraft).toHaveBeenCalledWith({ ...draft, worktree: false });
});

const previewAnswer = (worktree = true): ResolvedOptions => ({
  effective: {
    main: { value: null, source: null },
    spotter: { value: null, source: null },
    triage: { value: null, source: null },
    effort: { value: null, source: "provider_default" },
    temperature: { value: null, source: "provider_default" },
  },
  issues: [],
  worktree,
});

test("directory disclosure commits a path without discarding settings and collapses afterward", async () => {
  const draft = { ...emptyDraft(alpha.cwd), temperature: 42 };
  const props = await render({ draft });
  await click(host.querySelector<HTMLElement>(".directory-disclosure")!);
  const input = host.querySelector<HTMLInputElement>("#launch-directory")!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )!.set!.call(input, "/tmp/new-project");
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await click(host.querySelector<HTMLElement>(".new-session-other .primary")!);
  expect(props.onDraft).toHaveBeenCalledWith({
    ...draft,
    cwd: "/tmp/new-project",
    config: null,
  });
  expect(props.onNavigate).toHaveBeenCalledWith("settings");
  expect(host.querySelector("#launch-directory-form")).toBeNull();
});
