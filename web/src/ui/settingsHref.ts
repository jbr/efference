export function settingsHref(
  pathname: string,
  search: string,
  open: boolean,
): string {
  const params = new URLSearchParams(search);
  if (open) params.set("settings", "");
  else params.delete("settings");
  const query = params.toString();
  return pathname + (query ? `?${query}` : "");
}
