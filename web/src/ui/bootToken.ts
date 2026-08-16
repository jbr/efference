// The `#token=` fragment is read and scrubbed once, at bundle load — not in
// the login view. When the browser already holds a valid cookie the login view
// never mounts, and a scrub that lives there would leave the burned token
// sitting in the URL bar and browser history (docs/web-bind.md wants that
// residue gone regardless of how auth resolves).

function take(): string | null {
  const token = new URLSearchParams(location.hash.slice(1)).get("token");
  if (token) history.replaceState(null, "", location.pathname);
  return token;
}

let bootToken = take();

/** The boot fragment's token, surrendered to the first caller. */
export function takeBootToken(): string | null {
  const token = bootToken;
  bootToken = null;
  return token;
}

/** Re-read on a later hashchange (`efference open` into an open tab). */
export const takeFragmentToken = take;
