// The confirm rule behind the list's destructive verb (docs/web-client.md), and
// the markup a row carries. Rendered to static markup rather than through a DOM:
// the subscription is an effect and does not run there, which is what makes the
// seeded listing the whole of what these assert on.

import { expect, test } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import type { SessionEntry } from "../api/http.ts";
import { SessionList } from "./SessionList.tsx";
import { archiveWarning } from "./sessionRow.ts";

const row = (over: Partial<SessionEntry> = {}): SessionEntry => ({
  id: "alpha",
  title: "alpha's work",
  status: "idle",
  processes: 0,
  ...over,
});

test("only a session with something to lose is confirmed", () => {
  expect(archiveWarning(row())).toBe(null);
  expect(archiveWarning(row({ status: "running" }))).toBe("is mid-turn");
  expect(archiveWarning(row({ status: "gated" }))).toBe(
    "is holding a call at a gate",
  );
  expect(archiveWarning(row({ status: "stalled" }))).toBe(
    "stalled and has not been answered",
  );
  expect(archiveWarning(row({ processes: 1 }))).toBe(
    "holds 1 background process",
  );
  expect(archiveWarning(row({ processes: 3 }))).toBe(
    "holds 3 background processes",
  );
  expect(archiveWarning(row({ status: "running", processes: 2 }))).toBe(
    "is mid-turn and holds 2 background processes",
  );
});

test("a card links into the session and carries its status", () => {
  const markup = renderToStaticMarkup(
    <SessionList
      sessions={[row({ status: "running", processes: 2, cwd: "/Users/x/p" })]}
    />,
  );
  expect(markup).toContain('href="/sessions/alpha/"');
  expect(markup).toContain("attention-working");
  expect(markup).toContain("2 running");
  expect(markup).toContain("~/p");
});

test("an empty listing says so rather than showing an empty grid", () => {
  const markup = renderToStaticMarkup(<SessionList sessions={[]} />);
  expect(markup).toContain("No active sessions");
});
