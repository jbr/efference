// Latest transport failure and retry progress (docs/web-client.md).
import type { UpstreamFailure, UpstreamRetry } from "../api/types.ts";
import { upstreamCategory, upstreamMessage } from "./upstreamMessage.ts";

export interface UpstreamChip {
  text: string;
  title: string;
  retrying: boolean;
  progress: string | null;
}

export function retryText(retry: UpstreamRetry): string {
  const surface = retry.surface === "peek" ? "triage · " : "";
  return `${surface}Retrying · attempt ${retry.attempt} of ${retry.budget}`;
}

export function upstreamChip(
  failure: UpstreamFailure | null,
  retry: UpstreamRetry | null,
): UpstreamChip | null {
  if (failure === null && retry === null) return null;
  return {
    text: failure === null ? "" : upstreamMessage(failure.detail),
    title:
      failure === null
        ? ""
        : `${upstreamCategory(failure.status)}${failure.status === null ? "" : ` · HTTP ${failure.status}`}`,
    retrying: retry !== null,
    progress: retry === null ? null : retryText(retry),
  };
}
