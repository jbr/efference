// The context meter and the session cost (docs/web-client.md): an endpoint
// that reported no window sends `limit: null`, which draws the count alone
// instead of throwing, and a provider that prices no requests sends no cost,
// which draws no readout beside the meter.

import { describe, expect, it, test } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import type { UsageView } from "../api/types.ts";
import { TopBar } from "./TopBar.tsx";

const usage = (
  context: UsageView["context"],
  cost: UsageView["cost"] = null,
): UsageView =>
  ({
    last: null,
    cumulative: {
      input_tokens: 0,
      output_tokens: 0,
      cache_read_input_tokens: 0,
      cache_creation_input_tokens: 0,
    },
    last_request_at: null,
    context,
    cost,
    limits: {},
    cold_streak: 0,
  }) as unknown as UsageView;

const bar = (context: UsageView["context"], cost: UsageView["cost"] = null) =>
  renderToStaticMarkup(
    <TopBar
      listHref={null}
      identity={null}
      usage={usage(context, cost)}
      upstreamFailure={null}
      upstreamRetry={null}
    />,
  );

test("a known window draws a bar and used/limit", () => {
  const markup = bar({ limit: 200_000, used: 50_000 });
  expect(markup).toContain("meter-fill");
  expect(markup).toContain("50.0k/200k");
});

test("an unknown window draws the count alone, with no bar", () => {
  const markup = bar({ limit: null, used: 12_345 });
  expect(markup).not.toContain("meter-fill");
  expect(markup).toContain("12.3k");
  expect(markup).toContain("window unknown");
});

test("header no longer exposes the session panel", () => {
  expect(bar(null)).not.toContain("drawer-toggle");
});

test("a priced session draws its cost beside the meter", () => {
  const markup = bar(
    { limit: 200_000, used: 50_000 },
    { usd: 0.1, unpriced: 0 },
  );
  expect(markup).toContain("cost-meter");
  expect(markup).toContain("$0.10");
});

test("a session on a provider that prices nothing draws no cost", () => {
  expect(bar({ limit: 200_000, used: 50_000 })).not.toContain("cost-meter");
});

test("an unpriced total is a marked floor, its count only in the title", () => {
  const markup = bar(
    { limit: 200_000, used: 50_000 },
    { usd: 1.5, unpriced: 3 },
  );
  expect(markup).toContain(">~$1.50<");
  expect(markup).toContain("cost: ~$1.50 · 3 unpriced");
});

describe("transport notice", () => {
  it("shows one readable explanation below the header and clears it on recovery", () => {
    const failure = {
      status: 503,
      count: 4,
      detail:
        '{"error":{"message":"Temporarily overloaded.","code":"service_overloaded"}}',
    };
    const html = renderToStaticMarkup(
      <TopBar
        listHref={null}
        identity={null}
        usage={null}
        upstreamFailure={failure}
        upstreamRetry={null}
      />,
    );
    expect(html).toContain('role="status"');
    expect(html).toContain("Temporarily overloaded.");
    expect(html).toContain("HTTP 503");
    expect(html).not.toContain("service_overloaded");
    expect(html).not.toContain("×4");
    const recovered = renderToStaticMarkup(
      <TopBar
        listHref={null}
        identity={null}
        usage={null}
        upstreamFailure={null}
        upstreamRetry={null}
      />,
    );
    expect(recovered).not.toContain("upstream-notice");
  });
});

test("the host session offers a project launch in the tab row, but a daemon does not", () => {
  const props = {
    identity: null,
    usage: null,
    upstreamFailure: null,
    upstreamRetry: null,
  };
  const host = renderToStaticMarkup(
    <TopBar {...props} listHref="/" project="/work/a & b" />,
  );
  expect(host).toContain('<div class="session-tabs"><a class="session-new"');
  expect(host).toContain('aria-label="New session in this project"');
  expect(host).toContain('href="/?new=settings&amp;cwd=%2Fwork%2Fa+%26+b"');
  for (const markup of [
    renderToStaticMarkup(
      <TopBar {...props} listHref={null} project="/work/a" />,
    ),
    renderToStaticMarkup(<TopBar {...props} listHref="/" />),
  ])
    expect(markup).not.toContain("New session in this project");
});
