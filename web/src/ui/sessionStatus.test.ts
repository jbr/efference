import { expect, test, vi } from "vitest";
import type { Starting } from "../api/types.ts";
import { sessionStatus } from "./sessionStatus.ts";

const defaults = {
  starting: null,
  connection: "live",
  retrying: false,
  retryError: null,
  onRetry: vi.fn(),
  inspectionError: null,
} as const;
function starting(overrides: Partial<Starting> = {}): Starting {
  return {
    id: "deft-walnut",
    cwd: "/project",
    since: 0,
    resume: true,
    stage: "preparing",
    authorizing: null,
    failure: null,
    ...overrides,
  };
}

test("live sessions yield the slot to the turn; other connections occupy it", () => {
  expect(sessionStatus(defaults)).toBeNull();
  for (const connection of [
    "connecting",
    "reconnecting",
    "missing",
    "ended",
  ] as const) {
    expect(sessionStatus({ ...defaults, connection })).toEqual({
      text: `${connection}…`,
    });
  }
});

test("startup progress supersedes connection status and names preparation and authorization", () => {
  for (const [value, text] of [
    [starting({ stage: "environment" }), "running the env hook"],
    [starting(), "mounting MCP servers"],
    [
      starting({ authorizing: "github" }),
      "waiting for github to be authorized",
    ],
  ] as const) {
    const result = sessionStatus({
      ...defaults,
      connection: "connecting",
      starting: value,
    });
    expect(result?.text).toContain(text);
    expect(result?.error).toBeUndefined();
  }
});

test("startup failures retain recovery, retry refusal and pending state", () => {
  const failed = starting({
    failure: { reason: "MCP timed out", at: 1, withdrawn: [] },
  });
  const onRetry = vi.fn();
  const result = sessionStatus({
    ...defaults,
    starting: failed,
    onRetry,
    retryError: "host unavailable",
    inspectionError: "missing call",
  });
  expect(result).toMatchObject({
    text: "Could not start",
    error: true,
    detail: "MCP timed out\nhost unavailable",
    action: { label: "Retry", pending: false },
  });
  result?.action?.onClick();
  expect(onRetry).toHaveBeenCalledOnce();
  expect(
    sessionStatus({ ...defaults, starting: failed, retrying: true })?.action,
  ).toMatchObject({ label: "Retrying…", pending: true });
  expect(
    sessionStatus({ ...defaults, starting: { ...failed, resume: false } })
      ?.action?.label,
  ).toBe("Back to the form");
});

test("inspection error takes the slot until cleared, even during preparation", () => {
  expect(
    sessionStatus({
      ...defaults,
      starting: starting(),
      inspectionError: "missing call",
    }),
  ).toEqual({ text: "Could not inspect task: missing call", error: true });
  expect(
    sessionStatus({ ...defaults, inspectionError: "missing call" })?.error,
  ).toBe(true);
  expect(sessionStatus(defaults)).toBeNull();
});
