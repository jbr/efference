// Launch URL parsing and construction (docs/web-session-creation.md).
import type { Role } from "./pickerRows.ts";

export type LaunchPage = "location" | "settings" | Role;

export function launchPage(search: string): LaunchPage | null {
  const params = new URLSearchParams(search);
  const page = params.get("new");
  if (page === "location" || page === "settings") return page;
  const role = params.get("role");
  return page === "model" &&
    (role === "main" || role === "spotter" || role === "triage")
    ? role
    : null;
}

export function launchUrl(
  current: string,
  page: LaunchPage | null,
  cwd?: string,
): string {
  const url = new URL(current, "https://efference.invalid");
  url.searchParams.delete("new");
  url.searchParams.delete("role");
  if (page === "location" || page === "settings") {
    url.searchParams.set("new", page);
  } else if (page !== null) {
    url.searchParams.set("new", "model");
    url.searchParams.set("role", page);
  }
  if (page === null) url.searchParams.delete("cwd");
  else if (cwd !== undefined) url.searchParams.set("cwd", cwd);
  return `${url.pathname}${url.search}${url.hash}`;
}
