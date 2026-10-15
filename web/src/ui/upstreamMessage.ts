// Provider explanations for the transport notice (docs/web-client.md).
export function upstreamMessage(detail = ""): string {
  const start = detail.indexOf("{");
  if (start >= 0) {
    try {
      const body: unknown = JSON.parse(detail.slice(start));
      if (body !== null && typeof body === "object" && "error" in body) {
        const error = body.error;
        if (typeof error === "string") return bounded(error);
        if (
          error !== null &&
          typeof error === "object" &&
          "message" in error &&
          typeof error.message === "string"
        )
          return bounded(error.message);
      }
      if (
        body !== null &&
        typeof body === "object" &&
        "message" in body &&
        typeof body.message === "string"
      )
        return bounded(body.message);
    } catch {
      /* Non-JSON response bodies use the transport explanation below. */
    }
  }
  // A rejected SSE response can contain the entire echoed request. Keep only
  // the negotiation explanation, never its event data or unrecognized JSON.
  const explanation = detail
    .split(/\s[—–]\s|\r?\n|\{/u)[0]
    .trim()
    .replace(/:\s*$/, "");
  return bounded(explanation || "The upstream request failed.");
}

function bounded(message: string): string {
  const text = message.replace(/\s+/gu, " ").trim();
  return text.length > 600
    ? `${text.slice(0, 599)}…`
    : text || "The upstream request failed.";
}

export function upstreamCategory(status: number | null): string {
  if (status === null) return "Generation interrupted";
  if (status === 429) return "Usage or rate limit";
  if (status === 401 || status === 403) return "Access denied";
  if (status === 408 || status === 504) return "Upstream timeout";
  if (status >= 500) return "Upstream unavailable";
  return "Request rejected";
}
