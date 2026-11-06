import { expect, test } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import {
  GalleryTranscriptHeader,
  SessionChromeSpecimens,
} from "./SessionChromeSpecimens.tsx";
import { SessionStatusFixture } from "./SessionStatusFixture.tsx";

test("routine startup specimens use the production composer status", () => {
  for (const [state, text] of [
    ["Environment", "running the env hook"],
    ["Mounting MCP", "mounting MCP servers"],
    ["Authorization", "waiting for github to be authorized"],
  ] as const) {
    const html = renderToStaticMarkup(<SessionStatusFixture state={state} />);
    expect(html).toContain('role="status"');
    expect(html).toContain(text);
    expect(html).not.toContain('role="alert"');
    expect(html).not.toContain('class="turn-state');
  }
});

test("failure specimens keep recovery next to an editable draft", () => {
  const failure = renderToStaticMarkup(
    <SessionStatusFixture state="Retry refused" />,
  );
  expect(failure).toContain('role="alert"');
  expect(failure).toContain("Could not start");
  expect(failure).toContain("Reconnect before retrying.");
  expect(failure).toContain("Retry</button>");
  expect(failure).toContain('<textarea aria-label="Session status draft"');
  expect(failure).not.toContain("disabled");
  const local = renderToStaticMarkup(
    <SessionStatusFixture state="Inspection failed" />,
  );
  expect(local).not.toContain('class="turn-state');
  expect(local).toContain("Could not inspect task:");
  expect(local).toContain('aria-label="Interrupt current operation"');
  expect(
    renderToStaticMarkup(<SessionStatusFixture state="Reconnecting" />),
  ).toContain("reconnecting…");
  expect(
    renderToStaticMarkup(<SessionStatusFixture state="Your turn" />),
  ).toContain('class="turn-state');
});

test("session chrome contains no top banner or prototype comparison", () => {
  const html = renderToStaticMarkup(<SessionChromeSpecimens />);
  expect(html).not.toContain('class="banner"');
  expect(html).not.toContain("prototype");
  expect(html).toContain('class="composer-status"');
});

test("scene header uses real meters and fixture tabs without session links", () => {
  const html = renderToStaticMarkup(<GalleryTranscriptHeader />);
  expect(html).toContain("Worktree controls in the web UI");
  expect(html).toContain("147k/272k");
  expect(html).toContain("$0.42");
  expect(html).toContain("App settings");
  expect(html).toContain("switcher-tab dot-working is-current");
  expect(html).not.toMatch(/<a\b[^>]*\shref="\/sessions\//);
});
