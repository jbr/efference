// The `#claim=` fragment a sign-in link carries (docs/web-bind.md) is read and
// scrubbed once, at bundle load — not in the sign-in view. When the page already
// holds a valid bearer (api/bearer.ts) the sign-in view never mounts, and a scrub
// that lived there would leave the claim sitting in the URL bar and browser
// history; the page keeps its token and lets the link's sign-in expire unclaimed.

function take(): string | null {
  const claim = new URLSearchParams(location.hash.slice(1)).get("claim");
  if (claim) history.replaceState(null, "", location.pathname);
  return claim;
}

let bootClaim = take();

/** The boot fragment's claim, surrendered to the first caller. */
export function takeBootClaim(): string | null {
  const claim = bootClaim;
  bootClaim = null;
  return claim;
}

/** Re-read on a later hashchange (`efference dev open` into an open tab). */
export const takeFragmentClaim = take;
