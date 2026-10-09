import { useEffect, useState } from "react";

/** The wall clock in Unix millis, re-read once a second while `active`.
 * Topics carry absolute instants so the daemon does not re-emit a ticking
 * field (docs/control-api.md); this is the client's half. */
export function useNow(active: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    // The first read is a timer, not a synchronous set: the stored value may
    // be from before the clock was last needed.
    const refresh = () => setNow(Date.now());
    const first = setTimeout(refresh, 0);
    const id = setInterval(refresh, 1000);
    return () => {
      clearTimeout(first);
      clearInterval(id);
    };
  }, [active]);
  return now;
}
