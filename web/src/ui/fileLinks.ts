// Links inside a file the viewer renders (docs/web-client.md "The file viewer").
// A relative link in a repository's markdown names another path under the same
// working directory, so the viewer follows it in place rather than letting the
// browser resolve it against the page URL, which names no file at all.

/** Where a link in the file at `from` points: a path under the working
 * directory, and whether it was spelled as a directory (`inbox/`). `null` for
 * anything that is not a relative path — a scheme, a protocol-relative or
 * root-absolute URL, a bare fragment — or that climbs out of the root. */
export function resolveLink(
  from: string,
  href: string,
): { path: string; dir: boolean } | null {
  if (/^[a-z][a-z0-9+.-]*:/i.test(href) || href.startsWith("/")) return null;
  const bare = href.replace(/[?#].*$/, "");
  if (bare === "") return null;
  let decoded: string;
  try {
    decoded = decodeURIComponent(bare);
  } catch {
    return null;
  }
  const segments = from.split("/").slice(0, -1);
  for (const segment of decoded.split("/")) {
    if (segment === "" || segment === ".") continue;
    if (segment === "..") {
      if (segments.length === 0) return null;
      segments.pop();
    } else segments.push(segment);
  }
  return { path: segments.join("/"), dir: decoded.endsWith("/") };
}
