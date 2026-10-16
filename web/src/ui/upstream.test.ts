import { describe, expect, it } from "vitest";
import type { UpstreamRetry } from "../api/types.ts";
import { upstreamChip } from "./upstream.ts";
import { upstreamMessage } from "./upstreamMessage.ts";

const retry: UpstreamRetry = {
  surface: "turn",
  attempt: 2,
  budget: 12,
  next_attempt_at: 100,
  message: "retrying",
  detail: "old error",
};

describe("upstream notice", () => {
  it("is absent after recovery when no retry remains", () => {
    expect(upstreamChip(null, null)).toBeNull();
  });
  it("shows the latest explanation and retry progress without an accumulating count", () => {
    const chip = upstreamChip(
      {
        status: 503,
        count: 3,
        detail:
          '{"error":{"code":"service_overloaded","message":"Please retry."}}',
      },
      retry,
    );
    expect(chip?.text).toBe("Please retry.");
    expect(chip?.title).toBe("Upstream unavailable · HTTP 503");
    expect(chip?.progress).toBe("Retrying · attempt 2 of 12");
  });
  it("does not resurrect the retry's old error after a 2xx clears failure", () => {
    expect(upstreamChip(null, retry)?.text).toBe("");
    expect(upstreamChip(null, retry)?.progress).toContain("Retrying");
  });
  it("shows stream failures without inventing an HTTP status", () => {
    const chip = upstreamChip(
      { status: null, count: 1, detail: "connection reset" },
      null,
    );
    expect(chip?.title).toBe("Generation interrupted");
    expect(chip?.text).toBe("connection reset");
  });
  it("distinguishes triage retries", () => {
    expect(
      upstreamChip(null, { ...retry, surface: "triage" })?.progress,
    ).toContain("triage");
  });
});

describe("provider explanations", () => {
  it.each([
    {
      error: {
        type: "invalid_request_error",
        message: "Unsupported thinking mode.",
      },
      request_id: "req_1",
    },
    {
      error: {
        type: "usage_limit_reached",
        message: "The usage limit has been reached",
        resets_at: 100,
      },
    },
    {
      error: {
        code: "service_overloaded",
        message: "Temporarily overloaded.",
        param: null,
        type: "server_error",
      },
    },
    {
      type: "error",
      error: {
        type: "api_error",
        error_type: "server",
        message: "Stream closed.",
      },
      metadata: { provider_name: "provider" },
    },
    {
      error: {
        code: "credit_balance_exhausted",
        type: "insufficient_quota",
        message: "Add credits.",
      },
      sequence_number: 2,
    },
  ])("extracts error.message from %j", (body) => {
    expect(upstreamMessage(`upstream error: ${JSON.stringify(body)}`)).toBe(
      body.error.message,
    );
  });
  it("supports string errors and top-level messages", () => {
    expect(upstreamMessage('{"error":"Unavailable"}')).toBe("Unavailable");
    expect(upstreamMessage('{"message":"Unavailable"}')).toBe("Unavailable");
  });
  it("does not expose echoed instructions from a rejected SSE response", () => {
    expect(
      upstreamMessage(
        'Unexpected content-type for SSE request: None — event: response.created\ndata: {"response":{"instructions":"private"}}',
      ),
    ).toBe("Unexpected content-type for SSE request: None");
  });
  it("keeps non-JSON diagnostics bounded and excludes unknown JSON", () => {
    expect(upstreamMessage("connection reset")).toBe("connection reset");
    expect(upstreamMessage('{"unexpected":"private"}')).toBe(
      "The upstream request failed.",
    );
    expect(upstreamMessage("x".repeat(1000))).toHaveLength(600);
  });
});
