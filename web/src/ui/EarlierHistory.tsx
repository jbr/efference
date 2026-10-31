// Earlier windows preserve the visible transcript node (docs/web-client.md).
import {
  useEffect,
  useLayoutEffect,
  useRef,
  useSyncExternalStore,
} from "react";
import { captureHistoryAnchor } from "./historyAnchor.ts";
import type { LogEntry } from "../api/entry.ts";
import type { HistoryLoader } from "../session/history.ts";
import { subscribeTail, tailFollowing } from "./followTail.ts";

export function EarlierHistory({
  history,
  entries,
}: {
  history: HistoryLoader;
  entries: LogEntry[];
}) {
  const status = useSyncExternalStore(history.subscribe, history.snapshot);
  const following = useSyncExternalStore(subscribeTail, tailFollowing);
  const row = useRef<HTMLDivElement>(null);
  const restore = useRef<(() => void) | null>(null);
  const first = entries[0]?.seq;
  useLayoutEffect(() => {
    restore.current?.();
    restore.current = null;
  }, [first]);
  useEffect(() => {
    const element = row.current;
    if (
      !element ||
      (following &&
        document.documentElement.scrollHeight > window.innerHeight + 1) ||
      status.loading ||
      status.done ||
      status.error ||
      first === undefined
    )
      return;
    const observer = new IntersectionObserver(
      (observations) => {
        if (
          element.isConnected &&
          history.snapshot().error === null &&
          observations.some((observation) => observation.isIntersecting)
        ) {
          void history.load(() => {
            restore.current = captureHistoryAnchor();
          });
        }
      },
      { rootMargin: "300px 0px 0px" },
    );
    observer.observe(element);
    return () => observer.disconnect();
  }, [history, following, status, first]);
  if (first === undefined) return null;
  return (
    <div className="col earlier-history" ref={row}>
      {status.error ? (
        <>
          <span role="status">Could not load earlier entries.</span>{" "}
          <button
            type="button"
            onClick={() =>
              void history.load(() => {
                restore.current = captureHistoryAnchor();
              })
            }
          >
            Retry
          </button>
        </>
      ) : (
        <span role="status">
          {status.loading
            ? "Loading earlier entries…"
            : status.done
              ? "Beginning of session"
              : "Scroll up for earlier entries"}
        </span>
      )}
    </div>
  );
}
