// The new-session form's rules (docs/web-client.md): the request a draft
// makes, what a field says its value came from, when a preview reply is still
// about the draft on screen, and when Start may be pressed.

import { expect, test } from "vitest";
import type { ProjectEntry } from "../api/sessionForm.ts";
import {
  canStart,
  locationFor,
  mostRecent,
  nextRequest,
  sessionRequest,
  showsConfig,
  sourceWords,
  startRefusal,
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

test("a draft asks for its directory, and names a configuration only when it has one", () => {
  expect(sessionRequest({ cwd: "/tmp/one", config: null })).toEqual({
    cwd: "/tmp/one",
  });
  expect(
    sessionRequest({ cwd: "/tmp/one", config: "/tmp/one/alt.toml" }),
  ).toEqual({ cwd: "/tmp/one", config: "/tmp/one/alt.toml" });
});

test("the request carries no environment: the host establishes it", () => {
  // The whole of what creating from a browser changes. A client cannot supply
  // an environment worth supplying, and an omitted one is a different request
  // from an empty one (docs/control-api.md, docs/env.md).
  expect(
    Object.keys(sessionRequest({ cwd: "/tmp/one", config: null })),
  ).toEqual(["cwd"]);
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
  const draft = { cwd: "/tmp/one", config: null };
  const answered = { request: 2, answer: { issues: [] } };

  // No location yet: nothing to create.
  expect(startRefusal(null, answered, 2)).toBe("Pick a location");
  // Nothing has answered.
  expect(startRefusal(draft, null, 2)).toBe("Resolving…");
  // Answered, but for a draft that has moved on.
  expect(startRefusal(draft, answered, 3)).toBe("Resolving…");
  // Answered for this draft, with nothing wrong: pressable.
  expect(startRefusal(draft, answered, 2)).toBe(null);
});

test("a preview that flagged something refuses Start", () => {
  const draft = { cwd: "/tmp/one", config: null };
  const answered = {
    request: 1,
    answer: { issues: [{ field: "model", message: "no such model" }] },
  };
  expect(startRefusal(draft, answered, 1)).toBe("Fix what is marked above");
  expect(canStart(draft, answered, 1, false)).toBe(false);
});

test("a creation in flight refuses a second one", () => {
  const draft = { cwd: "/tmp/one", config: null };
  const answered = { request: 1, answer: { issues: [] } };
  // Nothing is wrong with the draft; the button is refused because pressing it
  // again would mint a second session (creation is not idempotent).
  expect(canStart(draft, answered, 1, false)).toBe(true);
  expect(canStart(draft, answered, 1, true)).toBe(false);
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

test("a typed directory stays on screen as the chosen row", () => {
  const projects = [entry()];
  // Typed into *Other directory…*: not in the index, so it is shown ahead of
  // it rather than leaving the form looking as though nothing is selected.
  const rows = withTypedLocation(projects, {
    cwd: "/tmp/scratch",
    config: null,
  });
  expect(rows).toHaveLength(2);
  expect(rows[0].cwd).toBe("/tmp/scratch");
  // The empty key is what marks it not remembered, and so not forgettable.
  expect(rows[0].config_key).toBe("");
  // A remembered directory is shown once, as itself.
  expect(
    withTypedLocation(projects, { cwd: projects[0].cwd, config: null }),
  ).toBe(projects);
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
