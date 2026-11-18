import { join } from "./base.ts";
import { apiFetch, throwIfFailed } from "./http.ts";
import type { AgentCandidates, PathCandidates } from "./types.ts";

async function discover<T>(
  base: string,
  kind: "paths" | "agents",
  prefix: string,
  signal: AbortSignal,
): Promise<T> {
  const query = new URLSearchParams({ prefix, limit: "50" });
  const response = await apiFetch(
    join(`/v1/discovery/${kind}?${query}`, base),
    {
      signal,
    },
  );
  await throwIfFailed(response);
  return (await response.json()) as T;
}

export const discoverPaths = (
  base: string,
  prefix: string,
  signal: AbortSignal,
) => discover<PathCandidates>(base, "paths", prefix, signal);
export const discoverAgents = (
  base: string,
  prefix: string,
  signal: AbortSignal,
) => discover<AgentCandidates>(base, "agents", prefix, signal);
