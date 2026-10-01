// The new-session form's rules (docs/web-client.md): the request a draft
// makes, what a field says its value came from, when a preview reply is still
// about the draft on screen, and when Start may be pressed.

import { expect, test } from "vitest";
import type { ProjectEntry, SessionPreview } from "../api/sessionForm.ts";
import {
  EFFORTS,
  type Draft,
  canStart,
  draftFor,
  emptyDraft,
  fieldSource,
  hasIssue,
  locationFor,
  modelText,
  mostRecent,
  nextRequest,
  orphanIssues,
  rowKey,
  sessionRequest,
  showsConfig,
  sourceWords,
  startRefusal,
  temperatureOf,
  triageText,
  withTypedLocation,
} from "./sessionForm.ts";

const entry = (over: Partial<ProjectEntry> = {}): ProjectEntry => ({
  label: "efference",
  cwd: "/Users/jbr/code/efference",
  config: null,
  config_key: "/Users/jbr/code/efference/efference.toml",
  last_used_at: 100,
  ...over,
});

/** A draft through the constructor production uses, the way the tests are meant
 * to build inputs. */
const draft = (over: Partial<Draft> = {}): Draft => ({
  ...emptyDraft("/tmp/one"),
  ...over,
});

/** A preview answer with nothing flagged, or with the issues named. */
const answer = (issues: SessionPreview["issues"] = []): SessionPreview =>
  ({
    effective: {
      main: { value: null, source: null },
      spotter: { value: null, source: null },
      triage: { value: null, source: null },
      effort: { value: null, source: "provider_default" },
      temperature: { value: null, source: "provider_default" },
    },
    issues,
  }) as SessionPreview;

test("a draft asks for its directory, and names a configuration only when it has one", () => {
  expect(sessionRequest(draft())).toEqual({
    cwd: "/tmp/one",
  });
  expect(sessionRequest(draft({ config: "/tmp/one/alt.toml" }))).toEqual({
    cwd: "/tmp/one",
    config: "/tmp/one/alt.toml",
  });
});

test("the request carries no environment: the host establishes it", () => {
  // The whole of what creating from a browser changes. A client cannot supply
  // an environment worth supplying, and an omitted one is a different request
  // from an empty one (docs/control-api.md, docs/env.md).
  expect(Object.keys(sessionRequest(draft()))).toEqual(["cwd"]);
});

test("an override is sent; an untouched field is not sent at all", () => {
  // Inheriting sends nothing, which is what makes an inherited default
  // distinguishable from an explicit one on the wire — and why the form posts
  // its overrides rather than the effective values it was shown.
  expect(sessionRequest(draft())).toEqual({ cwd: "/tmp/one" });
  expect(
    sessionRequest(
      draft({
        effort: "high",
        temperature: 40,
        main: { connection: "openrouter", model: "z-ai/gl" },
        spotter: { connection: "typesafe", model: "jev-latest" },
        triage: { mode: "separate", connection: "openai", model: "gpt-5" },
      }),
    ),
  ).toEqual({
    cwd: "/tmp/one",
    effort: "high",
    temperature: 40,
    main_connection: "openrouter",
    main_model: "z-ai/gl",
    spotter_connection: "typesafe",
    spotter_model: "jev-latest",
    triage: { mode: "separate", connection: "openai", model: "gpt-5" },
  });
});

test("triage sharing main sends nothing of its own", () => {
  expect(sessionRequest(draft({ triage: { mode: "inherit" } }))).toEqual({
    cwd: "/tmp/one",
  });
});

test("an issue replaces the source it sits on", () => {
  const issues = [
    { field: "temperature", code: "out_of_range", message: "must be 0 to 100" },
  ];
  // A field that failed has neither value nor source, so the message is all
  // the row has to say — and it says it in the source's place.
  expect(fieldSource("temperature", null, issues)).toBe("must be 0 to 100");
  expect(fieldSource("effort", "user_default", issues)).toBe("your default");
  expect(hasIssue(issues, "temperature")).toBe(true);
  expect(hasIssue(issues, "effort")).toBe(false);
});

test("an issue is said once — by its row, or beside Start", () => {
  const issues = [
    { field: "temperature", code: "out_of_range", message: "must be 0 to 100" },
    { field: "config", code: "invalid_config", message: "will not load" },
  ];
  // The row states its own problem where the operator is editing it; repeating
  // it under the button would say it twice. A configuration belongs to the
  // location, which is displayed rather than edited, so it has no row.
  expect(orphanIssues(issues)).toEqual([
    { field: "config", code: "invalid_config", message: "will not load" },
  ]);
  expect(orphanIssues([])).toEqual([]);
});

test("a resolved value is shown as the slug a reader recognizes", () => {
  expect(
    modelText({
      connection: "openrouter",
      provider: "openrouter",
      model: "z-ai/gl",
    }),
  ).toBe("openrouter/z-ai/gl");
  expect(modelText(null)).toBe(null);
  // Sharing main is the source's to say; repeating the model in the value
  // column would state it twice.
  expect(triageText({ mode: "shared" })).toBe(null);
  expect(
    triageText({
      mode: "separate",
      connection: "openai",
      provider: "openai",
      model: "gpt-5",
    }),
  ).toBe("openai/gpt-5");
});

test("the effort levels are the terminal form's, and the first is no override", () => {
  expect(EFFORTS.map((level) => level.label)).toEqual([
    "Use default",
    "low",
    "medium",
    "high",
    "xhigh",
    "max",
  ]);
  expect(EFFORTS[0].value).toBe(null);
});

test("an empty temperature means the default, and a bad one is not clamped", () => {
  expect(temperatureOf("")).toBe(null);
  expect(temperatureOf("   ")).toBe(null);
  expect(temperatureOf("0")).toBe(0);
  expect(temperatureOf("100")).toBe(100);
  // Sent as typed: the host reports it as an issue, which refuses Start and
  // names itself, where a silent clamp would launch at another temperature.
  expect(temperatureOf("150")).toBe(150);
  expect(temperatureOf("abc")).toBe(null);
});

test("each precedence branch has its own words", () => {
  expect(sourceWords("request")).toBe("chosen");
  expect(sourceWords("user_default")).toBe("your default");
  expect(sourceWords("project_config")).toBe("project config");
  expect(sourceWords("main_provider")).toBe("main's provider");
  expect(sourceWords("provider_default")).toBe("provider default");
  expect(sourceWords("shared_main")).toBe("shares main");
  // A branch the docs' list omits but the host sends (docs/control-api.md).
  expect(sourceWords("history")).toBe("a past pick");
  expect(sourceWords("decision_connection")).toBe("a decision model");
  // A field that failed resolves has no source at all.
  expect(sourceWords(null)).toBe(null);
});

test("a committed edit moves the request on, so a reply in flight is stale", () => {
  const first = nextRequest(0);
  const second = nextRequest(first);
  expect(first).toBe(1);
  expect(second).toBe(2);
  // The point of the counter: a reply still in flight for `first` no longer
  // names the draft on screen once `second` has gone out.
  expect(second).not.toBe(first);
});

test("Start waits for an answer about the draft on screen", () => {
  const current = draft();
  const answered = { request: 2, answer: answer() };

  // No location yet: nothing to create.
  expect(startRefusal(null, answered, 2)).toBe("Pick a location");
  // Nothing has answered.
  expect(startRefusal(current, null, 2)).toBe("Resolving…");
  // Answered, but for a draft that has moved on.
  expect(startRefusal(current, answered, 3)).toBe("Resolving…");
  // Answered for this draft, with nothing wrong: pressable.
  expect(startRefusal(current, answered, 2)).toBe(null);
});

test("a preview that flagged something refuses Start", () => {
  const current = draft();
  const answered = {
    request: 1,
    answer: answer([
      { field: "model", code: "invalid_selection", message: "no such model" },
    ]),
  };
  expect(startRefusal(current, answered, 1)).toBe("Fix what is marked above");
  expect(canStart(current, answered, 1, false)).toBe(false);
});

test("a creation in flight refuses a second one", () => {
  const current = draft();
  const answered = { request: 1, answer: answer() };
  // Nothing is wrong with the draft; the button is refused because pressing it
  // again would mint a second session (creation is not idempotent).
  expect(canStart(current, answered, 1, false)).toBe(true);
  expect(canStart(current, answered, 1, true)).toBe(false);
});

test("a workspace + opens on that directory, carrying its remembered config", () => {
  const named = entry({
    cwd: "/Users/jbr/code/two",
    config: "/Users/jbr/code/two/alt.toml",
    config_key: "/Users/jbr/code/two/alt.toml",
    last_used_at: 300,
  });
  const projects = [named, entry()];

  expect(locationFor(projects, "/Users/jbr/code/two")).toEqual({
    cwd: "/Users/jbr/code/two",
    config: "/Users/jbr/code/two/alt.toml",
  });
  // A directory with no remembered entry is still launchable — the index
  // filters what creation would refuse, never what it accepts — so the
  // workspace's own directory opens with the configuration left to probe.
  expect(locationFor(projects, "/Users/jbr/code/unknown")).toEqual({
    cwd: "/Users/jbr/code/unknown",
    config: null,
  });
});

test("the top bar opens on the most recently used location", () => {
  const projects = [entry({ last_used_at: 300 }), entry({ last_used_at: 100 })];
  expect(mostRecent(projects)).toBe(projects[0]);
  expect(mostRecent([])).toBe(undefined);
});

test("a clicked row selects that row's own configuration", () => {
  const plain = entry({ config_key: "builtin-default" });
  const named = entry({
    config: "/Users/jbr/code/efference/alt.toml",
    config_key: "/Users/jbr/code/efference/alt.toml",
  });

  // Two rows of one directory. Reading the directory instead of the row would
  // answer with the most recent entry's configuration, so clicking the second
  // selected the first and highlighted neither.
  expect(draftFor(named)).toEqual({
    cwd: "/Users/jbr/code/efference",
    config: "/Users/jbr/code/efference/alt.toml",
  });
  expect(draftFor(plain)).toEqual({
    cwd: "/Users/jbr/code/efference",
    config: null,
  });
});

test("two directories launched without a configuration do not collide", () => {
  // Both are stored under the same builtin-default key, so a key of
  // config_key alone would render them under one key.
  const one = entry({ cwd: "/Users/jbr/code/one" });
  const two = entry({ cwd: "/Users/jbr/code/two" });
  expect(one.config_key).toBe(two.config_key);
  expect(rowKey(one)).not.toBe(rowKey(two));
  // A named configuration still keys by both halves.
  const named = entry({
    cwd: "/Users/jbr/code/one",
    config: "/Users/jbr/code/one/alt.toml",
    config_key: "/Users/jbr/code/one/alt.toml",
  });
  expect(rowKey(named)).not.toBe(rowKey(one));
});

test("a typed directory stays on screen as the chosen row", () => {
  const projects = [entry()];
  // Typed into *Other directory…*: not in the index, so it is shown ahead of
  // it rather than leaving the form looking as though nothing is selected.
  const rows = withTypedLocation(projects, draft({ cwd: "/tmp/scratch" }));
  expect(rows).toHaveLength(2);
  expect(rows[0].cwd).toBe("/tmp/scratch");
  // The empty key is what marks it not remembered, and so not forgettable.
  expect(rows[0].config_key).toBe("");
  // A remembered directory is shown once, as itself.
  expect(withTypedLocation(projects, draft({ cwd: projects[0].cwd }))).toBe(
    projects,
  );
  // Nothing drafted yet: the index alone.
  expect(withTypedLocation(projects, null)).toBe(projects);
});

test("a row says its configuration only when it has one or shares a directory", () => {
  const one = entry();
  expect(showsConfig([one], one)).toBe(false);

  const named = entry({
    config: "/Users/jbr/code/efference/alt.toml",
    config_key: "/Users/jbr/code/efference/alt.toml",
  });
  expect(showsConfig([one], named)).toBe(true);
  // The index keys an entry by directory plus configuration, so one directory
  // is two rows that would otherwise read alike.
  expect(showsConfig([one, named], one)).toBe(true);
});
