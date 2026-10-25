import { describe, expect, it } from "vitest";
import type {
  InferenceIdentity,
  ModelSelection,
  SessionIdentity,
} from "../api/types.ts";
import {
  keeping,
  liveTarget,
  recordedFromSelections,
  switchFields,
  triageSeparate,
  triageValue,
} from "./switchModel.ts";

const selection = (connection: string, model: string): ModelSelection => ({
  connection,
  provider: "anthropic",
  model,
});

const recorded: InferenceIdentity = {
  main: selection("anthropic", "opus"),
  spotter: selection("anthropic", "haiku"),
  triage: { ...selection("anthropic", "opus"), shared: true },
};

describe("what a switch sends", () => {
  it("sends nothing while every role keeps the recorded selection", () => {
    expect(switchFields(keeping, recorded)).toBeNull();
  });

  it("names the main model when it moves", () => {
    expect(
      switchFields(
        { ...keeping, main: { connection: "openrouter", model: "gemini" } },
        recorded,
      ),
    ).toEqual({ connection: "openrouter", model: "gemini" });
  });

  it("treats a pick that repeats the recorded selection as no move", () => {
    expect(
      switchFields(
        { ...keeping, main: { connection: "anthropic", model: "opus" } },
        recorded,
      ),
    ).toBeNull();
  });

  it("names the spotter under its own prefix", () => {
    expect(
      switchFields(
        { ...keeping, spotter: { connection: "anthropic", model: "opus" } },
        recorded,
      ),
    ).toEqual({ spotter_connection: "anthropic", spotter_model: "opus" });
  });

  it("leaves triage alone when it already rides the main client", () => {
    expect(
      switchFields({ ...keeping, triage: { kind: "shared" } }, recorded),
    ).toBeNull();
  });

  it("sends triage shared when it had a client of its own", () => {
    const separate: InferenceIdentity = {
      ...recorded,
      triage: { ...selection("anthropic", "haiku"), shared: false },
    };
    expect(
      switchFields({ ...keeping, triage: { kind: "shared" } }, separate),
    ).toEqual({
      triage: { mode: "shared" },
    });
  });

  it("sends triage separate with its connection", () => {
    expect(
      switchFields(
        {
          ...keeping,
          triage: {
            kind: "separate",
            selection: { connection: "openrouter", model: "gemini" },
          },
        },
        recorded,
      ),
    ).toEqual({
      triage: { mode: "separate", connection: "openrouter", model: "gemini" },
    });
  });

  it("sends nothing for a session whose state is not known yet", () => {
    expect(switchFields(keeping, null)).toBeNull();
  });
});

describe("the roles a dormant session records", () => {
  it("reads every role from the flat selections", () => {
    expect(
      recordedFromSelections({
        connection: "anthropic",
        model: "opus",
        spotter_connection: "anthropic",
        spotter_model: "haiku",
        triage: { mode: "shared" },
      }),
    ).toEqual({
      main: { connection: "anthropic", model: "opus" },
      spotter: { connection: "anthropic", model: "haiku" },
      triage: { connection: "anthropic", model: "opus", shared: true },
    });
  });

  it("splits a separate triage that names only the slug", () => {
    expect(
      recordedFromSelections({
        triage: {
          mode: "separate",
          model: "openrouter/gemini",
          thinking: true,
          auto_choice: false,
        },
      }),
    ).toEqual({
      triage: { connection: "openrouter", model: "gemini", shared: false },
    });
  });

  it("treats a slug-read triage as the selection a pick repeats", () => {
    const roles = recordedFromSelections({
      triage: {
        mode: "separate",
        model: "openrouter/gemini",
        thinking: true,
        auto_choice: false,
      },
    });
    expect(
      switchFields(
        {
          ...keeping,
          triage: {
            kind: "separate",
            selection: { connection: "openrouter", model: "gemini" },
          },
        },
        roles,
      ),
    ).toBeNull();
  });

  it("keeps a role the log does not record absent", () => {
    expect(
      recordedFromSelections({ connection: "anthropic", model: "opus" }),
    ).toEqual({ main: { connection: "anthropic", model: "opus" } });
  });

  it("reads a listing with no selections as no roles", () => {
    expect(recordedFromSelections(null)).toBeNull();
  });
});

describe("what the triage row shows", () => {
  const shared = recordedFromSelections({
    connection: "anthropic",
    model: "opus",
    triage: { mode: "shared" },
  });
  const separate = recordedFromSelections({
    connection: "anthropic",
    model: "opus",
    triage: {
      mode: "separate",
      connection: "openrouter",
      model: "gemini",
      thinking: true,
      auto_choice: false,
    },
  });

  it("shows the source's answer while triage rides main", () => {
    expect(triageValue(keeping, shared)).toBe("Same as main model");
    expect(triageSeparate(keeping, shared)).toBe(false);
  });

  it("shows a client of its own when the log records one", () => {
    expect(triageValue(keeping, separate)).toBe("openrouter/gemini");
    expect(triageSeparate(keeping, separate)).toBe(true);
  });

  it("shows nothing chosen when the log records no triage", () => {
    expect(triageValue(keeping, null)).toBeNull();
    expect(triageSeparate(keeping, null)).toBe(false);
  });

  it("shows the pick once a client of its own is chosen", () => {
    const chosen = {
      ...keeping,
      triage: {
        kind: "separate" as const,
        selection: { connection: "deepseek", model: "deepseek-flash" },
      },
    };
    expect(triageValue(chosen, shared)).toBe("deepseek/deepseek-flash");
    expect(triageSeparate(chosen, shared)).toBe(true);
  });
});

describe("the root a switch resumes at", () => {
  const identity: SessionIdentity = {
    id: "still-kestrel",
    model: "opus",
    cwd: "/project/.efference/worktrees/still-kestrel",
  };

  it("names the root the host serves the session under, not where it runs", () => {
    // The identity's `cwd` is the working directory; a resume naming it answers
    // `state_mismatch` from the host, which is how a switch silently failed.
    const target = liveTarget("still-kestrel", "/project", identity, null, 10);
    expect(target.cwd).toBe("/project");
    expect(target.live).toBe(true);
    expect(target.recorded).toBe(null);
  });

  it("has no root to name when its listing row is not read yet", () => {
    expect(liveTarget("still-kestrel", null, identity, null, null).cwd).toBe(
      null,
    );
  });
});
