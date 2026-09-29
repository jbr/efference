// The context meter (docs/web-client.md): an endpoint that reported no window
// sends `limit: null`, which draws the count alone instead of throwing.

import { expect, test } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import type { Usage } from "../api/types.ts";
import { TopBar } from "./TopBar.tsx";

const usage = (context: Usage["context"]): Usage =>
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
    cost: null,
    limits: {},
    cold_streak: 0,
  }) as unknown as Usage;

const bar = (context: Usage["context"]) =>
  renderToStaticMarkup(
    <TopBar
      listHref={null}
      identity={null}
      usage={usage(context)}
      upstreamFailure={null}
      upstreamRetry={null}
      drawerOpen={false}
      attention={0}
      onDrawer={() => {}}
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
