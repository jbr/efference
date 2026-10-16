// @vitest-environment jsdom
import { expect, it } from "vitest";
import { acquireDraft } from "./drafts.ts";
import { launchedFrom, reopenLaunch, reopenedLaunch } from "./launchRetry.ts";
import { emptyDraft } from "./sessionForm.ts";

const draft = { ...emptyDraft("/work"), effort: "high" };

it("reopens the form on the draft that made a session, and carries the composer to the next", () => {
  launchedFrom("first", draft);
  reopenLaunch("first", "env: the hook exited 7", "the first ask");
  expect(reopenedLaunch()).toMatchObject({
    draft,
    reason: "env: the hook exited 7",
  });

  launchedFrom("second", draft);
  expect(acquireDraft("/sessions/second").snapshot().text).toBe(
    "the first ask",
  );
  expect(reopenedLaunch()).toBeNull();

  // A session this page did not create reopens at the location.
  reopenLaunch("elsewhere", "could not prepare session", "");
  expect(reopenedLaunch()?.draft).toBeNull();
});
