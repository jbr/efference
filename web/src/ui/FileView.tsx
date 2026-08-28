// The file viewer (docs/web-client.md): an overlay filling the band between
// the top bar and the dock, showing what a file *is* — read from disk over
// /files (docs/web-bind.md) — where the inspector shows what a call *did*.
// The transcript stays mounted behind it, one dismissal away; the dock stays
// visible and working, so the composer and gate remain at hand while
// reading.
//
// Two independent sources say where to look, and they answer different
// questions:
//
// - **What changed since you last looked** — the client keeps the bytes it last
//   showed per path and diffs each re-fetch against them, so a highlight means
//   exactly that, identical for an agent edit and a save from emacs. Empty on a
//   file's first open, which is every file follow switches you to.
// - **Where the edit landed** — the anchor carried by the edit's own result
//   (session/editAnchor.ts), located by content in the file as it is now. This
//   is what makes `open ⤢` land on the edit and follow land on each new one,
//   and it says when it is only approximately right rather than highlighting
//   confidently in the wrong place.
//
// Freshness is two sources folded into one mechanism: a committed edit in the
// log triggers an immediate conditional GET, and the daemon's change stream (an
// EventSource on the same /files URL) covers edits from outside — every poke,
// and every (re)open of the stream, answers with the same conditional GET, so
// the stream carries no content and replaces only the old poll's timer.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { fetchFile, fileStream } from "../api/files.ts";
import {
  locateAnchor,
  type AnchorMatch,
  type DiffAnchor,
} from "../session/editAnchor.ts";
import {
  diffLines,
  NO_CHANGES,
  type LineChanges,
} from "../session/lineDiff.ts";
import type { EditEvent, FileActivity } from "../session/touchedFiles.ts";
import { useFileTree } from "../session/useFileTree.ts";
import { langForPath } from "../render/parse.ts";
import { NumberedCode } from "./Code.tsx";
import { FileTreePane } from "./FileTree.tsx";
import { Markdown } from "./Markdown.tsx";
import { useNarrow } from "./useNarrow.ts";

interface Shown {
  text: string;
  changed: LineChanges;
  /** Where the edit that opened (or last landed on) this file is now, if one
   * was named. */
  anchored: AnchorMatch | null;
}

interface Refusal {
  code: string;
  message: string;
}

/** A scroll request. `demanded` distinguishes "you asked for this edit" — which
 * happens whether or not follow is on — from "the file moved under you", which
 * the follow toggle suppresses. */
interface ScrollTo {
  line: number;
  demanded: boolean;
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
    if (!(bar instanceof HTMLElement) || !(dock instanceof HTMLElement)) return;
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

/** What the viewer says about an anchor it could not place confidently. An
 * exact match says nothing: the highlight is the statement. */
function anchorNote(anchored: AnchorMatch): string | null {
  switch (anchored.kind) {
    case "exact":
      return null;
    case "approximate":
      return "position approximate — the file has changed since this edit";
    case "unlocated":
      return "this edit's lines are not in the file any more";
  }
}

/** What the viewer is currently loading and why. One value rather than a path
 * plus a mutable side channel: every reason to show a file — an `open ⤢`, a
 * tree click, follow landing on a new edit — is one of these, and `key` makes
 * "the same file again, for a different call's edit" a distinct request.
 *
 * Every load driven by one of these is unconditional. It carries an anchor to
 * resolve against the bytes, and a `304` would leave nothing to resolve
 * against — while the case it would save (an edit landed, fetch the file) is
 * exactly the case where the body comes back anyway. */
interface Target {
  path: string;
  anchor: DiffAnchor | null;
  /** The reader asked for this one — an `open ⤢`, a tree click, or an edit
   * arriving while follow is on. A jump for a demanded target happens whether
   * or not follow is on; the toggle governs the file moving under you. */
  demanded: boolean;
  key: number;
}

export function FileView({
  path,
  openKey,
  openAnchor,
  lastEdit,
  activity,
  closing,
  onSlideOut,
  onClose,
}: {
  /** The path the viewer was opened on, or `null` for the tree with no file
   * chosen — the `/tree` entry. Following may move off it. */
  path: string | null;
  /** Bumped by each open request, so re-opening the same path from a
   * different call re-runs its anchor rather than looking like no change. */
  openKey: number;
  /** Where in the file the call that opened this landed, if it was an edit
   * (session/editAnchor.ts). */
  openAnchor: DiffAnchor | null;
  /** The newest committed edit — the follow signal (session/touchedFiles.ts). */
  lastEdit: EditEvent | null;
  /** What the session has touched — the tree's annotations. */
  activity: ReadonlyMap<string, FileActivity>;
  closing: boolean;
  onSlideOut: () => void;
  onClose: () => void;
}) {
  const [target, setTarget] = useState<Target | null>(
    path === null ? null : { path, anchor: openAnchor, demanded: true, key: 0 },
  );
  const current = target?.path ?? null;

  const [shown, setShown] = useState<Shown | null>(null);
  const [refusal, setRefusal] = useState<Refusal | null>(null);
  const [follow, setFollow] = useState(true);
  const [showSource, setShowSource] = useState(false);
  const narrow = useNarrow();
  const [treeOpen, setTreeOpen] = useState(path === null);

  const etagRef = useRef<string | null>(null);
  const generationRef = useRef(0);
  /** The line the next render scrolls to, set when a diff or an anchor lands;
   * whether to honor it is the scroll effect's question, so `load` needs no
   * view of the follow toggle. */
  const scrollTargetRef = useRef<ScrollTo | null>(null);
  const bodyRef = useRef<HTMLDivElement>(null);

  const load = useCallback(
    async (
      target: string,
      conditional: boolean,
      /** Where the edit that asked for this file landed, resolved once the
       * bytes are here. */
      anchor: DiffAnchor | null,
      demanded: boolean,
    ) => {
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

      // The edit's own anchor, resolved against the bytes that just arrived.
      // It outranks the since-you-last-looked diff as a scroll target: the
      // reader asked for this edit, and on a first open the diff has nothing
      // to say.
      const anchored =
        anchor === null ? null : locateAnchor(anchor, result.text);

      setRefusal(null);
      setShown({ text: result.text, changed, anchored });
      if (anchored !== null && anchored.kind !== "unlocated") {
        scrollTargetRef.current = { line: anchored.line, demanded };
      } else if (changed.changed.length > 0 || changed.removedAt.length > 0) {
        scrollTargetRef.current = {
          line: changed.changed[0]?.[0] ?? changed.removedAt[0] + 1,
          demanded: false,
        };
      }
    },
    [],
  );

  // Every reason to show a file runs through one target, and always
  // unconditionally: the anchor needs bytes to resolve against, which a 304
  // would not carry.
  useEffect(() => {
    if (target === null) return;
    etagRef.current = null;
    void load(target.path, false, target.anchor, target.demanded);
  }, [target, load]);

  // The change stream (docs/web-bind.md): pokes for the open path, held only
  // while the tab is visible — the poll's `document.hidden` check, become
  // closing the stream. `onopen` re-fetches conditionally, which covers what
  // a hidden interval or a dropped connection missed (EventSource reconnects
  // on its own); a poke's answer is the same conditional GET. No anchor: a
  // change from outside the session is not an edit anyone pointed at.
  useEffect(() => {
    if (current === null) return;
    let source: EventSource | null = null;
    const open = () => {
      source = fileStream(current);
      source.addEventListener(
        "change",
        () => void load(current, true, null, false),
      );
      source.onopen = () => void load(current, true, null, false);
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

  // A fresh open request — a call's `open ⤢`, or the inspector's. The key
  // changes per request, so a second call's `open ⤢` on the file already shown
  // still re-runs with *its* anchor. Adjusted during render, the sanctioned
  // shape (App.tsx does the same for the drawers' `shown`).
  const [seenOpen, setSeenOpen] = useState(openKey);
  if (openKey !== seenOpen) {
    setSeenOpen(openKey);
    setTarget(
      path === null
        ? null
        : { path, anchor: openAnchor, demanded: true, key: openKey },
    );
    if (path !== null && narrow) setTreeOpen(false);
  }

  // Following the agent: a landed edit on the open path re-reads it now rather
  // than at the next poke; one elsewhere switches the view — unless following
  // is off, which suppresses the switch while the live reload keeps working.
  // Either way the edit's own anchor is what the next load points at: the edit
  // that just landed *is* the newest change, so its inserted lines are in the
  // bytes about to arrive and the match is exact. Seeding `seenSeq` with the
  // mount-time edit is what makes edits from before the view opened not news:
  // highlights mean "since you last looked", and before the view opened,
  // nobody looked.
  const [seenSeq, setSeenSeq] = useState<number | null>(lastEdit?.seq ?? null);
  if (lastEdit !== null && lastEdit.seq !== seenSeq) {
    setSeenSeq(lastEdit.seq);
    if (lastEdit.path === current || follow) {
      setTarget({
        path: lastEdit.path,
        anchor: lastEdit.anchor,
        // Follow off means the file may still reload under you, but nothing
        // moves the viewport.
        demanded: follow,
        key: lastEdit.seq,
      });
    }
  }

  // Scroll to the target once it is on screen. A demanded jump — an edit the
  // reader clicked, or the one follow just switched to — happens regardless of
  // the follow toggle; the toggle only governs the file moving under you. In
  // source view the anchor is the line's `data-line`; in rendered markdown it
  // is the first stamped block.
  useEffect(() => {
    const scroll = scrollTargetRef.current;
    if (scroll === null || shown === null) return;
    scrollTargetRef.current = null;
    if (!follow && !scroll.demanded) return;
    const anchor =
      bodyRef.current?.querySelector(`[data-line="${scroll.line}"]`) ??
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
  const tree = useFileTree();
  const lang = useMemo(() => langForPath(current ?? ""), [current]);
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
    const ranges =
      shown.anchored?.kind === "exact"
        ? [...shown.changed.changed, ...shown.anchored.changed]
        : shown.changed.changed;
    for (const [a, b] of ranges) {
      for (let n = a; n <= b; n++) changed.add(n);
    }
    // A pure deletion has no line to tint; the seam is drawn on the line
    // after it.
    const seams = new Set(shown.changed.removedAt.map((n) => n + 1));
    return (n: number) =>
      changed.has(n) ? "changed" : seams.has(n) ? "removed-before" : undefined;
  }, [shown]);

  const note = shown?.anchored == null ? null : anchorNote(shown.anchored);
  const onOpenFromTree = useCallback((chosen: string) => {
    setTarget((prev) => ({
      path: chosen,
      anchor: null,
      demanded: true,
      key: (prev?.key ?? 0) + 1,
    }));
  }, []);

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
        <button
          className={treeOpen ? "linkish follow on" : "linkish follow"}
          title="the working directory, and a path finder over it"
          onClick={() => setTreeOpen((open) => !open)}
        >
          files ⌸
        </button>
        <span className="file-view-title">current file</span>
        <span
          className="file-view-path"
          title="read from disk, as it is now — not the file at the time of any call"
        >
          {current ?? "—"}
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
      <div className="file-view-split">
        {treeOpen && (
          <FileTreePane
            tree={tree}
            activity={activity}
            current={current}
            onOpen={onOpenFromTree}
          />
        )}
        <div className="file-view-body" ref={bodyRef}>
          <div className="file-view-col">
            {current === null && (
              <div className="file-refusal">
                <div className="file-refusal-what">no file open</div>
                <div className="dim">
                  choose one from the working directory, or find it by path
                </div>
              </div>
            )}
            {refusal !== null && (
              <div className="file-refusal">
                <div className="file-refusal-what">{refusalText(refusal)}</div>
                <div className="dim">{refusal.message}</div>
              </div>
            )}
            {note !== null && <div className="anchor-note dim">{note}</div>}
            {shown !== null &&
              lines !== null &&
              (isMarkdown && !showSource ? (
                <Markdown
                  changedLines={
                    shown.anchored?.kind === "exact"
                      ? [...shown.changed.changed, ...shown.anchored.changed]
                      : shown.changed.changed
                  }
                >
                  {shown.text}
                </Markdown>
              ) : (
                <NumberedCode lines={lines} lang={lang} lineClass={lineClass} />
              ))}
          </div>
        </div>
      </div>
    </div>
  );
}
