// The file tree's machinery (docs/web-client.md "Navigating the working
// directory"): which directories are expanded, what each one contains, and the
// change stream that keeps that current.
//
// Session-layer rather than component-internal, per docs/web-client.md: a
// phone eventually renders this as something other than an indented list, and
// that should be a layout change over the same state.
//
// Lazy, one request per expanded directory — no recursive walk. The recursive
// listing exists (`listDir(path, depth)`) and the finder uses it; a tree that
// fetched it would pay for the whole project to show one directory.
//
// Each expanded directory also holds a directory-scope change stream
// (docs/web-bind.md), so a file that appears while you are looking at its
// parent shows up. As with the viewer, the answer to *stream open* is the same
// re-list as the answer to a poke: neither watch-then-subscribe order is
// airtight against a change landing during setup.

import { useCallback, useEffect, useRef, useState } from "react";
import { dirStream, listDir, type ChangeStream } from "../api/files.ts";
import type { DirEntry } from "../api/types.ts";

export interface DirState {
  entries: DirEntry[];
  /** A server cap was hit — only reachable at absurd directory sizes, and said
   * rather than silently truncated. */
  truncated: boolean;
  /** Why this directory has no entries, when that is the reason. */
  refusal: string | null;
  loading: boolean;
}

export interface FileTree {
  /** Listings by directory path; `""` is the root. */
  dirs: ReadonlyMap<string, DirState>;
  expanded: ReadonlySet<string>;
  toggle: (path: string) => void;
  /** Open a directory and every parent of it — how the finder and the viewer
   * reveal a path the reader arrived at some other way. */
  reveal: (path: string) => void;
}

/** The parents of a path, root first: `a/b/c` → `["", "a", "a/b"]`. */
export function ancestors(path: string): string[] {
  const segments = path.split("/").filter((s) => s !== "");
  const out = [""];
  for (let i = 1; i < segments.length; i++) {
    out.push(segments.slice(0, i).join("/"));
  }
  return out;
}

export function useFileTree(): FileTree {
  // The root is always open: a tree whose first interaction is "expand the
  // thing you are already looking at" has an empty state for no reason.
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(
    () => new Set([""]),
  );
  const [dirs, setDirs] = useState<ReadonlyMap<string, DirState>>(
    () => new Map(),
  );

  const load = useCallback(async (path: string) => {
    setDirs((prev) => {
      const next = new Map(prev);
      const at = next.get(path);
      next.set(path, {
        entries: at?.entries ?? [],
        truncated: at?.truncated ?? false,
        refusal: null,
        loading: true,
      });
      return next;
    });
    const result = await listDir(path);
    setDirs((prev) => {
      const next = new Map(prev);
      next.set(
        path,
        result.kind === "ok"
          ? {
              entries: result.listing.entries,
              truncated: result.listing.truncated,
              refusal: null,
              loading: false,
            }
          : {
              entries: [],
              truncated: false,
              refusal: result.message,
              loading: false,
            },
      );
      return next;
    });
  }, []);

  // One effect reconciling the whole stream set, rather than a hook per
  // directory — the expanded set is data, and the number of hooks cannot be.
  const streams = useRef(new Map<string, ChangeStream>());
  useEffect(() => {
    const open = streams.current;
    for (const [path, stream] of open) {
      if (!expanded.has(path)) {
        stream.close();
        open.delete(path);
      }
    }
    for (const path of expanded) {
      if (open.has(path)) continue;
      const answer = () => void load(path);
      open.set(path, dirStream(path, { onOpen: answer, onChange: answer }));
    }
  }, [expanded, load]);

  useEffect(() => {
    const open = streams.current;
    return () => {
      for (const stream of open.values()) stream.close();
      open.clear();
    };
  }, []);

  const toggle = useCallback((path: string) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }, []);

  const reveal = useCallback((path: string) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      for (const parent of ancestors(path)) next.add(parent);
      return next;
    });
  }, []);

  return { dirs, expanded, toggle, reveal };
}

export interface ProjectPaths {
  /** Every file path the recursive listing reached, root-relative. */
  paths: string[];
  loading: boolean;
  /** The server capped the walk: the set is a prefix of the project. */
  truncated: boolean;
}

/**
 * Every file path under the root, for the finder to match against — one
 * recursive listing, fetched when the finder first opens rather than at boot,
 * because a session that never opens it should not pay for the walk.
 *
 * Held for the client session once fetched: it is a list of names, the
 * directory streams the tree holds already correct the part on screen, and
 * re-walking the project on every keystroke-worth of palette is the thing this
 * shape exists to avoid.
 */
export function useProjectPaths(active: boolean): ProjectPaths {
  const [state, setState] = useState<ProjectPaths>({
    paths: [],
    loading: false,
    truncated: false,
  });
  const fetched = useRef(false);
  useEffect(() => {
    if (!active || fetched.current) return;
    fetched.current = true;
    setState((prev) => ({ ...prev, loading: true }));
    void listDir("", MAX_DEPTH).then((result) => {
      setState(
        result.kind === "ok"
          ? {
              paths: result.listing.entries
                .filter((entry) => entry.kind === "file")
                .map((entry) => entry.name),
              loading: false,
              truncated: result.listing.truncated,
            }
          : { paths: [], loading: false, truncated: false },
      );
    });
  }, [active]);
  return state;
}

/** Mirrors `list::MAX_DEPTH` (src/listeners/files/list.rs), which clamps anything
 * larger anyway — asking for the server's own cap keeps the two from drifting
 * into a client that silently gets less than it asked for. */
const MAX_DEPTH = 8;
