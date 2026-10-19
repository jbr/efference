// The file tree and its path filter (docs/web-client.md "Navigating the
// working directory") — the way to a file no call has named, which is the set
// you do not already know about.
//
// Both are views over session-layer state: `useFileTree` holds the listings and
// their change streams, `matchPaths` ranks. Nothing here fetches, so meeting
// the phone is a layout change over the same machinery.
//
// The annotations are derived from the transcript, not from the endpoint: files
// the session has read are the agent's **working set** and read bold; files it
// has edited carry the accent. Reads deliberately do not move the viewer, so
// this is where a read registers at all.

import { useMemo, useState } from "react";
import type { DirEntry } from "../api/files.ts";
import { matchPaths } from "../session/pathMatch.ts";
import type { FileActivity } from "../session/touchedFiles.ts";
import { useProjectPaths, type FileTree } from "../session/useFileTree.ts";

const join = (dir: string, name: string): string =>
  dir === "" ? name : `${dir}/${name}`;

/** The class carrying a path's session annotations, or undefined for a file
 * nothing has touched. */
function activityClass(
  activity: ReadonlyMap<string, FileActivity>,
  path: string,
): string | undefined {
  const at = activity.get(path);
  if (at === undefined) return undefined;
  return at.edited ? "touched edited" : "touched";
}

export function FileTreePane({
  tree,
  activity,
  current,
  onOpen,
}: {
  tree: FileTree;
  activity: ReadonlyMap<string, FileActivity>;
  /** The path the viewer is showing, marked in the tree. */
  current: string | null;
  onOpen: (path: string) => void;
}) {
  // An empty filter is the tree; any text replaces it with ranked paths over
  // the recursive listing, fetched the first time a filter is typed.
  const [query, setQuery] = useState("");
  const [cursor, setCursor] = useState(0);
  const filtering = query !== "";
  const { paths, loading, truncated } = useProjectPaths(filtering);
  const hits = useMemo(
    () => (filtering ? matchPaths(query, paths) : []),
    [filtering, query, paths],
  );
  const at = Math.min(cursor, Math.max(hits.length - 1, 0));
  const choose = (path: string) => {
    setQuery("");
    tree.reveal(path);
    onOpen(path);
  };

  return (
    <aside className="file-tree">
      <div className="file-filter">
        <input
          className="file-filter-input"
          type="text"
          autoFocus={fineEnough()}
          value={query}
          placeholder="filter by path"
          aria-label="filter by path"
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          onChange={(e) => {
            setQuery(e.target.value);
            setCursor(0);
          }}
          onKeyDown={(e) => {
            if (!filtering) return;
            if (e.key === "Escape") {
              // Clears the filter; on an empty field Escape falls through to
              // closing the sidebar.
              e.stopPropagation();
              setQuery("");
            } else if (e.key === "ArrowDown")
              setCursor(Math.min(at + 1, hits.length - 1));
            else if (e.key === "ArrowUp") setCursor(Math.max(at - 1, 0));
            else if (e.key === "Enter" && hits[at] !== undefined)
              choose(hits[at].path);
            else return;
            e.preventDefault();
          }}
        />
        {filtering && (
          <button
            className="file-filter-clear"
            aria-label="clear filter"
            onClick={() => setQuery("")}
          >
            <svg viewBox="0 0 16 16" aria-hidden="true">
              <circle cx="8" cy="8" r="8" />
              <path d="M5.5 5.5l5 5M10.5 5.5l-5 5" />
            </svg>
          </button>
        )}
      </div>
      <div className="file-tree-body">
        {!filtering ? (
          <DirNodes
            dir=""
            depth={0}
            tree={tree}
            activity={activity}
            current={current}
            onOpen={onOpen}
          />
        ) : (
          <>
            {hits.map((hit, i) => (
              <button
                key={hit.path}
                className={[
                  "file-tree-row",
                  i === at ? "at" : "",
                  activityClass(activity, hit.path) ?? "",
                ].join(" ")}
                onMouseEnter={() => setCursor(i)}
                onClick={() => choose(hit.path)}
              >
                <span className="file-tree-name">
                  <Highlighted text={hit.path} spans={hit.matched} />
                </span>
              </button>
            ))}
            {loading && (
              <div className="file-tree-note dim">listing the project…</div>
            )}
            {!loading && hits.length === 0 && (
              <div className="file-tree-note dim">no path matches</div>
            )}
            {truncated && (
              <div className="file-tree-note dim">
                ⋯ the project listing was capped; deeper paths are missing
              </div>
            )}
          </>
        )}
      </div>
    </aside>
  );
}

/** A path with the filter's matched spans marked — the reason the row is in
 * the list. */
function Highlighted({
  text,
  spans,
}: {
  text: string;
  spans: readonly [number, number][];
}) {
  const parts = [];
  let at = 0;
  for (const [start, end] of spans) {
    if (start > at) parts.push(text.slice(at, start));
    parts.push(<mark key={start}>{text.slice(start, end)}</mark>);
    at = end;
  }
  parts.push(text.slice(at));
  return <>{parts}</>;
}

/** Focus the filter on open where a keyboard is the likely next input; on a
 * touch screen focusing would raise the on-screen keyboard over the tree the
 * reader came to look at. */
const fineEnough = () =>
  typeof window !== "undefined" &&
  window.matchMedia("(hover: hover) and (pointer: fine)").matches;

function DirNodes({
  dir,
  depth,
  tree,
  activity,
  current,
  onOpen,
}: {
  dir: string;
  depth: number;
  tree: FileTree;
  activity: ReadonlyMap<string, FileActivity>;
  current: string | null;
  onOpen: (path: string) => void;
}) {
  const state = tree.dirs.get(dir);
  if (state === undefined || (state.loading && state.entries.length === 0)) {
    return <div className="file-tree-note dim">…</div>;
  }
  if (state.refusal !== null) {
    return <div className="file-tree-note dim">{state.refusal}</div>;
  }
  return (
    <>
      {state.entries.map((entry) => (
        <TreeRow
          key={entry.name}
          entry={entry}
          path={join(dir, entry.name)}
          depth={depth}
          tree={tree}
          activity={activity}
          current={current}
          onOpen={onOpen}
        />
      ))}
      {state.truncated && (
        <div className="file-tree-note dim">⋯ listing truncated</div>
      )}
    </>
  );
}

function TreeRow({
  entry,
  path,
  depth,
  tree,
  activity,
  current,
  onOpen,
}: {
  entry: DirEntry;
  path: string;
  depth: number;
  tree: FileTree;
  activity: ReadonlyMap<string, FileActivity>;
  current: string | null;
  onOpen: (path: string) => void;
}) {
  const isDir = entry.kind === "dir";
  const open = tree.expanded.has(path);
  const classes = ["file-tree-row"];
  if (path === current) classes.push("current");
  const annotation = activityClass(activity, path);
  if (annotation !== undefined && !isDir) classes.push(annotation);
  return (
    <>
      <button
        className={classes.join(" ")}
        style={{ paddingInlineStart: `${0.4 + depth * 0.9}rem` }}
        title={path}
        onClick={() => (isDir ? tree.toggle(path) : onOpen(path))}
      >
        <span className="file-tree-mark">
          {isDir ? (open ? "▾" : "▸") : ""}
        </span>
        <span className="file-tree-name">{entry.name}</span>
        {entry.symlink && <span className="dim"> ↦</span>}
      </button>
      {isDir && open && (
        <DirNodes
          dir={path}
          depth={depth + 1}
          tree={tree}
          activity={activity}
          current={current}
          onOpen={onOpen}
        />
      )}
    </>
  );
}
