// @vitest-environment jsdom
// The switch page (docs/resumption.md): the only thing it does beyond rendering is
// turn a choice into a resume — of a running session, after archiving it, since a
// live session cannot be resumed and a resume is what reads a switch. A dormant
// session has no running state at all, so the page reads its own row for the roles
// it last ran on.
import { act, type ComponentProps } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import {
  archiveSession,
  fetchDormantSession,
  resumeSession,
} from "../api/http.ts";
import type {
  SessionIdentity,
  ArchivedSession,
  DormantSession,
} from "../api/types.ts";
import { SwitchModelPage } from "./SwitchModelPage.tsx";
import {
  archivedTarget,
  keeping,
  liveTarget,
  type SwitchTarget,
} from "./switchModel.ts";

vi.mock("../api/http.ts", () => ({
  archiveSession: vi.fn(),
  fetchDormantSession: vi.fn(),
  resumeSession: vi.fn(),
}));

// The root the host serves this session under, which a resume names: not the
// identity's `cwd`, which is where the session runs.
const project = "/Users/jbr/code/efference";

const identity: SessionIdentity = {
  id: "still-kestrel",
  title: "Smoke-testing worktrees",
  model: "opus",
  cwd: "/Users/jbr/code/efference/.efference/worktrees/still-kestrel",
  inference: {
    main: { connection: "anthropic", provider: "anthropic", model: "opus" },
    spotter: { connection: "anthropic", provider: "anthropic", model: "haiku" },
    triage: {
      connection: "anthropic",
      provider: "anthropic",
      model: "opus",
      shared: true,
    },
  },
};

// The same session once the host has archived it: the listing row says nothing
// about the roles, and the single-row read says all of it.
const listing: ArchivedSession = {
  id: identity.id,
  cwd: project,
  title: identity.title ?? undefined,
  state: "archived",
};

const dormant: DormantSession = {
  ...listing,
  context_tokens: 76_200,
  selections: {
    connection: "anthropic",
    model: "opus",
    spotter_connection: "anthropic",
    spotter_model: "haiku",
    triage: { mode: "shared" },
  },
};

const live = (over: Partial<SwitchTarget> = {}): SwitchTarget => ({
  ...liveTarget(identity.id, project, identity, null, 76_200),
  ...over,
});

let host: HTMLDivElement;
let root: Root;

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  vi.clearAllMocks();
  vi.mocked(archiveSession).mockResolvedValue({
    id: identity.id,
    state: "archived",
  });
  vi.mocked(fetchDormantSession).mockResolvedValue(dormant);
  vi.mocked(resumeSession).mockResolvedValue({
    id: identity.id,
    state: "active",
    resumed: true,
  });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

async function render(
  over: Partial<ComponentProps<typeof SwitchModelPage>> = {},
) {
  const props = {
    target: live(),
    onClose: vi.fn(),
    onSwitched: vi.fn(),
    onRestarting: vi.fn(),
    ...over,
  };
  await act(async () => {
    root.render(<SwitchModelPage {...props} />);
  });
  // A dormant target with no roles read yet settles on the effect's promise.
  await act(async () => {});
  return props;
}

const switchButton = () =>
  host.querySelector<HTMLButtonElement>("button.primary")!;

test("nothing chosen leaves the switch unavailable", async () => {
  await render();
  expect(switchButton().disabled).toBe(true);
});

test("a chosen model switches by archiving and then resuming", async () => {
  const props = await render({
    initialDraft: {
      ...keeping,
      main: { connection: "openrouter", model: "gemini" },
    },
  });
  expect(switchButton().disabled).toBe(false);
  await act(async () => switchButton().click());

  expect(archiveSession).toHaveBeenCalledWith(identity.id);
  expect(resumeSession).toHaveBeenCalledWith(identity.id, project, {
    connection: "openrouter",
    model: "gemini",
  });
  expect(props.onRestarting).toHaveBeenCalledWith(true);
  expect(
    vi.mocked(props.onRestarting!).mock.invocationCallOrder[0],
  ).toBeLessThan(vi.mocked(archiveSession).mock.invocationCallOrder[0]);
  const order = vi.mocked(archiveSession).mock.invocationCallOrder[0];
  expect(vi.mocked(resumeSession).mock.invocationCallOrder[0]).toBeGreaterThan(
    order,
  );
  expect(props.onSwitched).toHaveBeenCalled();
});

test("a failed archive stays on the page and resumes nothing", async () => {
  vi.mocked(archiveSession).mockRejectedValue(new Error("The host is busy."));
  const props = await render({
    initialDraft: {
      ...keeping,
      main: { connection: "openrouter", model: "gemini" },
    },
  });
  await act(async () => switchButton().click());

  expect(resumeSession).not.toHaveBeenCalled();
  expect(props.onSwitched).not.toHaveBeenCalled();
  expect(host.querySelector('[role="alert"]')?.textContent).toContain(
    "Switching still-kestrel",
  );
  expect(props.onRestarting).toHaveBeenLastCalledWith(false);
});

test("a cold session says the cache is already gone", async () => {
  await render({
    target: live({
      cold: { since: 1_760_000_000_000, context_tokens: 76_200 },
    }),
  });
  expect(host.textContent).toContain("already believed expired");
});

test("a dormant session reads its own row for the roles it ran on", async () => {
  await render({ target: archivedTarget(listing) });
  expect(fetchDormantSession).toHaveBeenCalledWith(identity.id);
  expect(host.textContent).toContain("anthropic/opus");
  expect(host.textContent).toContain("anthropic/haiku");
  expect(host.textContent).toContain("Same as main model");
});

test("a caller that already read the row saves the fetch", async () => {
  await render({ target: archivedTarget(listing, dormant) });
  expect(fetchDormantSession).not.toHaveBeenCalled();
  expect(host.textContent).toContain("anthropic/opus");
});

test("a dormant session resumes on the chosen models without an archive", async () => {
  const props = await render({
    target: archivedTarget(listing),
    initialDraft: {
      ...keeping,
      main: { connection: "openrouter", model: "gemini" },
    },
  });
  await act(async () => switchButton().click());

  expect(archiveSession).not.toHaveBeenCalled();
  expect(resumeSession).toHaveBeenCalledWith(identity.id, project, {
    connection: "openrouter",
    model: "gemini",
  });
  expect(props.onSwitched).toHaveBeenCalled();
});

test("a session whose log records no roles still offers every row", async () => {
  vi.mocked(fetchDormantSession).mockResolvedValue({
    ...dormant,
    selections: null,
  });
  await render({
    target: archivedTarget(listing),
    initialDraft: {
      ...keeping,
      main: { connection: "openrouter", model: "gemini" },
    },
  });
  expect(host.textContent).toContain("Choose a model");
  await act(async () => switchButton().click());
  expect(resumeSession).toHaveBeenCalledWith(identity.id, project, {
    connection: "openrouter",
    model: "gemini",
  });
});
