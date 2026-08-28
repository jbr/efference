// The file tree and the path finder (docs/web-client.md "Navigating the
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
  const [finding, setFinding] = useState(false);
  return (
    <aside className="file-tree">
      <div className="file-tree-head">
        <span className="file-view-title">working directory</span>
        <span className="spacer" />
        <button
          className="linkish"
          title="find a file by path"
          onClick={() => setFinding(true)}
        >
          find ⌕
        </button>
      </div>
      <div className="file-tree-body">
        <DirNodes
          dir=""
          depth={0}
          tree={tree}
          activity={activity}
          current={current}
          onOpen={onOpen}
        />
      </div>
      {finding && (
        <FileFinder
          activity={activity}
          onOpen={(path) => {
            setFinding(false);
            tree.reveal(path);
            onOpen(path);
          }}
          onClose={() => setFinding(false)}
        />
      )}
    </aside>
  );
}

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

/** The path palette: type a fragment, get ranked paths. Over the recursive
 * listing, which is fetched the first time this opens. */
function FileFinder({
  activity,
  onOpen,
  onClose,
}: {
  activity: ReadonlyMap<string, FileActivity>;
  onOpen: (path: string) => void;
  onClose: () => void;
}) {
  const [query, setQuery] = useState("");
  const [cursor, setCursor] = useState(0);
  const { paths, loading, truncated } = useProjectPaths(true);
  const hits = useMemo(() => matchPaths(query, paths), [query, paths]);
  const at = Math.min(cursor, Math.max(hits.length - 1, 0));

  return (
    <div className="file-finder" role="dialog" aria-label="find a file">
      <input
        className="file-finder-input"
        autoFocus
        value={query}
        placeholder={loading ? "listing the project…" : "path fragment"}
        onChange={(e) => {
          setQuery(e.target.value);
          setCursor(0);
        }}
        onKeyDown={(e) => {
          if (e.key === "Escape") onClose();
          else if (e.key === "ArrowDown")
            setCursor((n) => Math.min(n + 1, hits.length - 1));
          else if (e.key === "ArrowUp") setCursor((n) => Math.max(n - 1, 0));
          else if (e.key === "Enter" && hits[at] !== undefined)
            onOpen(hits[at].path);
          else return;
          e.preventDefault();
        }}
      />
      <div className="file-finder-hits">
        {hits.map((hit, i) => (
          <button
            key={hit.path}
            className={
              i === at
                ? `file-finder-hit at ${activityClass(activity, hit.path) ?? ""}`
                : `file-finder-hit ${activityClass(activity, hit.path) ?? ""}`
            }
            onMouseEnter={() => setCursor(i)}
            onClick={() => onOpen(hit.path)}
          >
            {hit.path}
          </button>
        ))}
        {!loading && hits.length === 0 && (
          <div className="file-tree-note dim">nothing matches</div>
        )}
        {truncated && (
          <div className="file-tree-note dim">
            ⋯ the project listing was capped; deeper paths are missing
          </div>
        )}
      </div>
    </div>
  );
}
