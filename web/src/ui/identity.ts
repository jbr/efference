// The top bar's session identity (docs/web-client.md): pure functions over the
// `session_identity` topic, so the component holds no path logic.
//
// The wire carries the working directory absolute, as the daemon knows it. A
// browser cannot read `$HOME`, so the home prefix is recognized by shape —
// `/Users/<name>` and `/home/<name>`, the two layouts this runs on — and only
// ever as whole path components. Anything it does not recognize is shown
// verbatim: an unshortened absolute path is correct, a wrongly shortened one is
// a claim about a directory that does not exist.

const HOME_PARENTS = ["Users", "home"];

/** `/Users/jbr/code/efference` → `~/code/efference`; anything else verbatim. */
export function homeRelative(cwd: string): string {
  const parts = cwd.split("/");
  // ["", parent, user, ...rest] — an absolute path with at least a user segment.
  if (
    parts.length < 3 ||
    parts[0] !== "" ||
    !HOME_PARENTS.includes(parts[1]) ||
    parts[2] === ""
  ) {
    return cwd;
  }
  const rest = parts.slice(3);
  return rest.length === 0 ? "~" : `~/${rest.join("/")}`;
}
