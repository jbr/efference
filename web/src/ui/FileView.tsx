// The file viewer (docs/web-client.md): an overlay filling the band between
// the top bar and the dock, showing what a file *is* — read from disk over
// /files (docs/web-bind.md) — where the inspector shows what a call *did*.
// The transcript stays mounted behind it, one dismissal away; the dock stays
// visible and working, so the composer and gate remain at hand while
// reading.
//
// Highlights come from diffing successive fetches, never from the tool call:
// the client keeps the bytes it last showed per path and diffs each re-fetch
// against them, so a highlight means "what changed since you last looked" —
// identical for an agent edit and a save from emacs. Freshness is two
// sources folded into one mechanism: a committed edit in the log triggers an
// immediate conditional GET, and the daemon's change stream (an EventSource
// on the same /files URL) covers edits from outside — every poke, and every
// (re)open of the stream, answers with the same conditional GET, so the
// stream carries no content and replaces only the old poll's timer.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { fetchFile, fileStream } from "../api/files.ts";
import {
  diffLines,
  NO_CHANGES,
  type LineChanges,
} from "../session/lineDiff.ts";
import type { EditEvent } from "../session/touchedFiles.ts";
import { langForPath } from "../render/parse.ts";
import { NumberedCode } from "./Code.tsx";
import { Markdown } from "./Markdown.tsx";

interface Shown {
  text: string;
  changed: LineChanges;
}

interface Refusal {
  code: string;
  message: string;
}

/** The bytes last rendered, per path — the diff baseline. Module-scoped
 * rather than component state so "since you last looked" survives closing
 * and reopening the viewer: the reader did look, whether or not the overlay
 * stayed mounted. Client-session-scoped, like everything derived here. */
const lastShown = new Map<string, string>();

/** The band between the sticky chrome: below the top bar, above the dock.
 * Both edges are measured, not guessed in CSS — the dock's height moves
 * (the panel expands and collapses, the textarea resizes) — the same shape
 * as the gate float's dock offset (Gate.tsx). */
function useChromeBand(): { top: number; bottom: number } {
  const [band, setBand] = useState({ top: 0, bottom: 0 });
  useEffect(() => {
    const bar = document.querySelector(".top-bar");
    const dock = document.querySelector(".dock");
    if (!(bar instanceof HTMLElement) || !(dock instanceof HTMLElement))
      return;
    const update = () =>
      setBand({ top: bar.offsetHeight, bottom: dock.offsetHeight });
    update();
    const observer = new ResizeObserver(update);
    observer.observe(bar);
    observer.observe(dock);
    return () => observer.disconnect();
  }, []);
  return band;
}

/** The refusal, legible against the path — never a blank pane. The endpoint
 * refuses rather than degrades (docs/web-bind.md), so every body that does
 * arrive is whole and there is no truncation state here. */
function refusalText(refusal: Refusal): string {
  switch (refusal.code) {
    case "too_large":
      return "too large to view";
    case "unsupported_content":
      return "not a text file";
    case "not_found":
      return "no such file under the working directory";
    default:
      return "could not read this file";
  }
}

export function FileView({
  path,
  lastEdit,
  closing,
  onSlideOut,
  onClose,
}: {
  /** The path the viewer was opened on; following may move off it. */
  path: string;
  /** The newest committed edit — the follow signal (session/touchedFiles.ts). */
  lastEdit: EditEvent | null;
  closing: boolean;
  onSlideOut: () => void;
  onClose: () => void;
}) {
  const [current, setCurrent] = useState(path);
  // Reopening from an `open ⤢` while already open re-seeds the path —
  // adjusted during render, the sanctioned shape (App.tsx does the same for
  // the drawers' `shown`).
  const [seed, setSeed] = useState(path);
  if (path !== seed) {
    setSeed(path);
    setCurrent(path);
  }

  const [shown, setShown] = useState<Shown | null>(null);
  const [refusal, setRefusal] = useState<Refusal | null>(null);
  const [follow, setFollow] = useState(true);
  const [showSource, setShowSource] = useState(false);

  const etagRef = useRef<string | null>(null);
  const generationRef = useRef(0);
  /** The line the next render scrolls to, set when a diff lands; whether to
   * honor it is the scroll effect's question, so `load` needs no view of the
   * follow toggle. */
  const scrollTargetRef = useRef<number | null>(null);
  const bodyRef = useRef<HTMLDivElement>(null);

  const load = useCallback(async (target: string, conditional: boolean) => {
    const generation = ++generationRef.current;
    const result = await fetchFile(
      target,
      conditional ? etagRef.current : null,
    );
    // A newer load (a path switch, a faster poll) supersedes this one.
    if (generation !== generationRef.current) return;
    if (result.kind === "unchanged") return;
    if (result.kind === "refused") {
      // Drop the validator: a file restored with its old mtime (a `mv` back)
      // would otherwise revalidate 304 against the refusal screen forever.
      etagRef.current = null;
      setRefusal(result);
      setShown(null);
      return;
    }
    etagRef.current = result.etag;
    const prev = lastShown.get(target);
    const changed =
      prev === undefined ? NO_CHANGES : diffLines(prev, result.text);
    lastShown.set(target, result.text);
    setRefusal(null);
    setShown({ text: result.text, changed });
    if (changed.changed.length > 0 || changed.removedAt.length > 0) {
      scrollTargetRef.current =
        changed.changed[0]?.[0] ?? changed.removedAt[0] + 1;
    }
  }, []);

  // Opening a path: unconditional fetch, fresh validator.
  useEffect(() => {
    etagRef.current = null;
    void load(current, false);
  }, [current, load]);

  // The change stream (docs/web-bind.md): pokes for the open path, held only
  // while the tab is visible — the poll's `document.hidden` check, become
  // closing the stream. `onopen` re-fetches conditionally, which covers what
  // a hidden interval or a dropped connection missed (EventSource reconnects
  // on its own); a poke's answer is the same conditional GET.
  useEffect(() => {
    let source: EventSource | null = null;
    const open = () => {
      source = fileStream(current);
      source.addEventListener("change", () => void load(current, true));
      source.onopen = () => void load(current, true);
    };
    const visibility = () => {
      if (document.hidden) {
        source?.close();
        source = null;
      } else if (source === null) {
        open();
      }
    };
    if (!document.hidden) open();
    document.addEventListener("visibilitychange", visibility);
    return () => {
      source?.close();
      document.removeEventListener("visibilitychange", visibility);
    };
  }, [current, load]);

  // Following the agent: a landed edit on the open path re-fetches now
  // rather than at the next poll; one elsewhere switches the view — unless
  // following is off, which suppresses the switch and the scroll while the
  // live reload keeps working. Seeding `seenSeq` with the mount-time edit is
  // what makes edits from before the view opened not news: highlights mean
  // "since you last looked", and before the view opened, nobody looked.
  // Render-time adjustment again; the fetch itself is `refetch`'s effect.
  const [seenSeq, setSeenSeq] = useState<number | null>(lastEdit?.seq ?? null);
  const [refetch, setRefetch] = useState(0);
  if (lastEdit !== null && lastEdit.seq !== seenSeq) {
    setSeenSeq(lastEdit.seq);
    if (lastEdit.path === current) setRefetch((n) => n + 1);
    else if (follow) setCurrent(lastEdit.path);
  }
  useEffect(() => {
    if (refetch === 0) return;
    void load(current, true);
    // `current` is deliberately not a dependency: a path switch does its own
    // unconditional load above, and re-firing a stale refetch at it would
    // race that.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refetch, load]);

  // Scroll to the first change once it is on screen — unless following is
  // off, which keeps the highlight and drops the jump. In source view the
  // anchor is the line's `data-line`; in rendered markdown it is the first
  // stamped block.
  useEffect(() => {
    const target = scrollTargetRef.current;
    if (target === null || shown === null) return;
    scrollTargetRef.current = null;
    if (!follow) return;
    const anchor =
      bodyRef.current?.querySelector(`[data-line="${target}"]`) ??
      bodyRef.current?.querySelector("[data-changed]");
    anchor?.scrollIntoView({ block: "center" });
  }, [shown, showSource, follow]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  const band = useChromeBand();
  const lang = useMemo(() => langForPath(current), [current]);
  const isMarkdown = lang === "markdown";

  const lines = useMemo<[number, string][] | null>(
    () =>
      shown === null
        ? null
        : shown.text.split("\n").map((line, i) => [i + 1, line]),
    [shown],
  );
  const lineClass = useMemo(() => {
    if (shown === null) return undefined;
    const changed = new Set<number>();
    for (const [a, b] of shown.changed.changed) {
      for (let n = a; n <= b; n++) changed.add(n);
    }
    // A pure deletion has no line to tint; the seam is drawn on the line
    // after it.
    const seams = new Set(shown.changed.removedAt.map((n) => n + 1));
    return (n: number) =>
      changed.has(n) ? "changed" : seams.has(n) ? "removed-before" : undefined;
  }, [shown]);

  return (
    <div
      className={closing ? "file-view closing" : "file-view"}
      style={{ top: band.top, bottom: band.bottom }}
      onTransitionEnd={(e) => {
        if (
          closing &&
          e.target === e.currentTarget &&
          e.propertyName === "transform"
        )
          onSlideOut();
      }}
    >
      <div className="file-view-head">
        <span className="file-view-title">current file</span>
        <span
          className="file-view-path"
          title="read from disk, as it is now — not the file at the time of any call"
        >
          {current}
        </span>
        <span className="spacer" />
        {isMarkdown && (
          <button className="linkish" onClick={() => setShowSource((s) => !s)}>
            {showSource ? "rendered" : "source"}
          </button>
        )}
        <button
          className={follow ? "linkish follow on" : "linkish follow"}
          title="switch to whichever file the agent edits and scroll to the change"
          onClick={() => setFollow((f) => !f)}
        >
          {follow ? "following ⇢" : "follow"}
        </button>
        <button className="linkish" onClick={onClose}>
          ✕
        </button>
      </div>
      <div className="file-view-body" ref={bodyRef}>
        <div className="file-view-col">
          {refusal !== null && (
            <div className="file-refusal">
              <div className="file-refusal-what">{refusalText(refusal)}</div>
              <div className="dim">{refusal.message}</div>
            </div>
          )}
          {shown !== null &&
            lines !== null &&
            (isMarkdown && !showSource ? (
              <Markdown changedLines={shown.changed.changed}>
                {shown.text}
              </Markdown>
            ) : (
              <NumberedCode lines={lines} lang={lang} lineClass={lineClass} />
            ))}
        </div>
      </div>
    </div>
  );
}
