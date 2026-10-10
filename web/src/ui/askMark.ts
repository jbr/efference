// What marks an `ask` in the transcript and composer (docs/web-client.md): whether
// the question is the one the composer's send answers, one still to come, or done.

import type { Ask } from "../api/types.ts";
import type { AskQuestion } from "../session/tree.ts";

export type AskStanding = "current" | "upcoming" | "asked";

/** Where a question stands, as far as this client can see. `resolved` is a
 * committed result; without one, the live `ask` topic decides — and with no topic
 * (a replay, a client that connected after the fact) nothing says more than
 * "asked". */
export function askStanding(
  ask: AskQuestion,
  live: Pick<Ask, "call_id"> | null,
): AskStanding {
  if (ask.resolved || live === null) return "asked";
  return live.call_id === ask.callId ? "current" : "upcoming";
}

const place = (ask: AskQuestion) =>
  ask.of > 1 ? ` · question ${ask.n} of ${ask.of}` : "";

export function askMarkText(ask: AskQuestion, standing: AskStanding): string {
  switch (standing) {
    case "current":
      return `answering${place(ask)}`;
    case "upcoming":
      return `next${place(ask)}`;
    case "asked":
      return `asked${place(ask)}`;
  }
}

/** The composer's hint while a question is pending: which one a send answers. */
export const replyHint = (ask: Pick<Ask, "n" | "of">): string =>
  ask.of > 1
    ? `your reply answers question ${ask.n} of ${ask.of}`
    : "your reply answers the question";
