import { useRef, useState } from "react";

/** Archive confirmation and request feedback (docs/web-client.md). */
export function ArchiveAction({
  id,
  warning,
  onArchive,
  onInteract,
}: {
  id: string;
  warning: string | null;
  onArchive: () => Promise<unknown>;
  onInteract?: () => void;
}) {
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const pending = useRef(false);

  async function archive() {
    if (pending.current) return;
    pending.current = true;
    setBusy(true);
    setError(null);
    try {
      await onArchive();
    } catch (failure) {
      setError(`Archiving ${id}: ${String(failure)}`);
    } finally {
      pending.current = false;
      setBusy(false);
    }
  }

  return (
    <div className="archive-action" aria-busy={busy}>
      {confirming && warning !== null && (
        <p className="archive-warning">
          {id} {warning}. Archiving stops the session and its background
          processes.
        </p>
      )}
      {error !== null && <p role="alert">{error}</p>}
      <div className="archive-buttons">
        <button
          type="button"
          className="archive-button"
          disabled={busy}
          onClick={() => {
            onInteract?.();
            if (warning === null || confirming) void archive();
            else setConfirming(true);
          }}
        >
          {busy
            ? "Archiving…"
            : confirming
              ? "Archive anyway"
              : "Archive session"}
        </button>
        {confirming && (
          <button
            type="button"
            disabled={busy}
            onClick={() => {
              setConfirming(false);
              setError(null);
            }}
          >
            Cancel
          </button>
        )}
      </div>
    </div>
  );
}
