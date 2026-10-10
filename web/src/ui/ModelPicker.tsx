// Role-specific model browsing in the launch flow (docs/web-session-creation.md).
// Matching and catalog refresh rules follow the terminal picker.
// Model information has a separate control so inspecting it does not choose a model.
//
// Catalog loading is the terminal form's too (docs/model-catalog.md): the
// stored listing on open, which never waits on a provider, then one awaited
// refresh per connection that has no usable copy, each answer replacing its
// connection's rows. A query that matches nothing under a connection chip asks
// for one fresh read, so a model released since the last read is there by the
// time its id is typed.
//
// The age clock is read once on open and passed down, so the facts are the same
// on every render — and what makes them testable.

import {
  Fragment,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  type CatalogEntry,
  fetchCatalog,
  fetchConnection,
} from "../api/sessionForm.ts";
import type { Chip } from "./matcher.ts";
import {
  type Role,
  type Row,
  catalogNodes,
  descends,
  rowKey,
  rows,
} from "./pickerRows.ts";
import { ModelPickerRow } from "./ModelPickerRow.tsx";
import "./ModelPicker.css";

/** How stale a copy may be before a no-match query reads the connection again
 * (docs/tui.md). The host raises anything under 60 to 60 regardless. */
const MISS_MAX_AGE = 60;

/** Browsing position belongs to one role within a launch flow, not to its
 * chosen model or to the host's catalog (docs/web-session-creation.md). */
export interface PickerBrowseState {
  query: string;
  chips: Chip[];
  selected: number;
  scrollTop: number;
}

export function ModelPicker({
  role,
  cwd,
  config,
  initialBrowse,
  onBrowse,
  onPick,
  onClose,
}: {
  role: Role;
  cwd: string;
  config: string | null;
  initialBrowse?: PickerBrowseState;
  onBrowse?: (state: PickerBrowseState) => void;
  /** A row was chosen: a model to set, or nothing for the typed-id row's
   * connection. */
  onPick: (selection: { connection: string; model: string }) => void;
  onClose: () => void;
}) {
  const [entries, setEntries] = useState<CatalogEntry[] | null>(null);
  const [query, setQuery] = useState(initialBrowse?.query ?? "");
  const [chips, setChips] = useState<Chip[]>(initialBrowse?.chips ?? []);
  const [selected, setSelected] = useState(initialBrowse?.selected ?? 0);
  const [keyboardNavigation, setKeyboardNavigation] = useState(false);
  const [focusSearch] = useState(
    () =>
      typeof window !== "undefined" &&
      typeof window.matchMedia === "function" &&
      window.matchMedia("(hover: hover) and (pointer: fine)").matches,
  );
  const rowList = useRef<HTMLDivElement>(null);
  const restored = useRef(false);
  const [initialScroll] = useState(initialBrowse?.scrollTop ?? 0);

  useEffect(() => {
    onBrowse?.({
      query,
      chips,
      selected,
      scrollTop:
        entries === null
          ? initialScroll
          : (rowList.current?.scrollTop ?? initialScroll),
    });
  }, [query, chips, selected, onBrowse, initialScroll, entries]);

  useLayoutEffect(() => {
    if (entries === null || restored.current || rowList.current === null)
      return;
    rowList.current.scrollTop = initialScroll;
    restored.current = true;
  }, [entries, initialScroll]);
  // One `max_age=60` read per connection per visit, so typing the same miss
  // twice does not ask the provider twice.
  const refreshed = useRef(new Set<string>());

  const replace = useCallback((connection: string, entry: CatalogEntry) => {
    setEntries((current) =>
      current === null
        ? current
        : current.map((one) => (one.connection === connection ? entry : one)),
    );
  }, []);

  useEffect(() => {
    let live = true;
    // Stored copies first: this never waits on a provider, so the picker opens
    // on whatever the host already has.
    fetchCatalog(cwd, config, role)
      .then((answer) => {
        if (!live) return;
        setEntries(answer.connections);
        // Then one awaited read per connection with no usable copy, each
        // replacing its own entry as it lands.
        for (const entry of answer.connections) {
          if (entry.status === "fresh" || entry.status === "unavailable") {
            continue;
          }
          fetchConnection(entry.connection, cwd, config, role)
            .then((fresh) => live && replace(entry.connection, fresh))
            .catch(() => {});
        }
      })
      .catch(() => live && setEntries([]));
    return () => {
      live = false;
    };
  }, [cwd, config, role, replace]);

  const nodes = useMemo(() => catalogNodes(entries ?? []), [entries]);
  const list_ = useMemo(
    () => rows(nodes, entries ?? [], chips, query, role),
    [nodes, entries, chips, query, role],
  );

  // A query that matches nothing under a connection chip asks for one fresh
  // read of that connection: a model released since the last read shows up by
  // the time its id is typed.
  useEffect(() => {
    const connection = chips.length === 1 ? chips[0] : null;
    if (
      entries === null ||
      connection === null ||
      query.trim() === "" ||
      /\s/.test(query.trim()) ||
      refreshed.current.has(connection) ||
      list_.some((row) => row.kind !== "typed")
    ) {
      return;
    }
    refreshed.current.add(connection);
    fetchConnection(connection, cwd, config, role, MISS_MAX_AGE)
      .then((entry) => replace(connection, entry))
      .catch(() => {});
  }, [entries, list_, chips, query, cwd, config, role, replace]);

  /** Set the query and return the selection to the first row (docs/tui.md): the
   * highlight is otherwise left over rows the new query does not have. Done at
   * the commit rather than in an effect, which is where the change happens. */
  const type = (next: string) => {
    setQuery(next);
    setSelected(0);
    setKeyboardNavigation(false);
  };

  const choose = (row: Row | undefined) => {
    if (row === undefined) return;
    onBrowse?.({
      query,
      chips,
      selected: list_.indexOf(row),
      scrollTop: rowList.current?.scrollTop ?? 0,
    });
    if (row.kind === "typed") {
      onPick({ connection: row.connection, model: row.model });
    } else if (row.kind === "model") {
      onPick({ connection: row.node.connection, model: row.node.model ?? "" });
    } else if (descends(row)) {
      setChips(
        row.kind === "group" && row.node.group !== undefined
          ? [row.node.connection, row.node.group]
          : [row.node.connection],
      );
      type("");
    }
  };

  const onKey = (event: React.KeyboardEvent) => {
    if (event.key === "Escape") {
      event.preventDefault();
      // Escape clears a non-empty query first, and only then returns to the
      // form, so one key does not discard a whole search.
      if (query !== "") type("");
      else if (chips.length > 0) {
        setChips((current) => current.slice(0, -1));
        type("");
      } else onClose();
      return;
    }
    if (!(event.target instanceof HTMLInputElement)) return;
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      setKeyboardNavigation(true);
      const step = event.key === "ArrowDown" ? 1 : -1;
      setSelected((current) =>
        list_.length === 0 ? 0 : (current + step + list_.length) % list_.length,
      );
      return;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      const row = list_[selected];
      // Enter descends into an interior row and picks a leaf, the terminal
      // form's rule.
      if (row !== undefined && row.kind !== "typed" && descends(row)) {
        choose(row);
      } else {
        choose(row);
        onClose();
      }
      return;
    }
    if (event.key === "Backspace" && query === "" && chips.length > 0) {
      // The keyboard index belongs to one namespace's result list.
      event.preventDefault();
      setChips((current) => current.slice(0, -1));
      type("");
    }
  };

  // The clock the age facts are computed against, read once when the picker
  // opens and held: an age that shifts on every render is not a fact, and
  // reading the clock during render is not allowed anyway. A picker left open
  // across a day shows a day-old age, which the next visit corrects.
  const [openedAt] = useState(() => Math.floor(Date.now() / 1000));

  const firstAuthor = list_.findIndex((row) => row.kind === "group");
  const sections = list_.map((row, index) => {
    if (query.trim() !== "") return null;
    if (chips.length === 0)
      return row.kind === "connection" ? "Browse connections" : "Recent models";
    if (chips.length === 1 && firstAuthor >= 0) {
      if (index < firstAuthor) return "Recent models";
      return row.kind === "group" ? "Browse authors" : "Other models";
    }
    return null;
  });

  const searchLabel =
    chips.length === 0 ? "Search models" : `Search in ${chips.join(" / ")}`;

  return (
    <div
      className="picker"
      onKeyDown={onKey}
      onPointerDown={() => setKeyboardNavigation(false)}
    >
      <nav className="picker-path" aria-label="Model location">
        <button
          className="quiet picker-root"
          disabled={chips.length === 0 && query === ""}
          onClick={() => {
            setChips([]);
            type("");
            if (rowList.current !== null) rowList.current.scrollTop = 0;
          }}
        >
          All models
        </button>
        <span aria-hidden="true"> / </span>
        {chips.map((chip, index) => (
          <Fragment key={`${index}-${chip}`}>
            <button
              className="quiet picker-breadcrumb"
              aria-label={`Browse ${chips.slice(0, index + 1).join(" / ")}`}
              aria-current={index === chips.length - 1 ? "location" : undefined}
              onClick={() => {
                setChips((current) => current.slice(0, index + 1));
                type("");
              }}
            >
              {chip}
            </button>
            <span aria-hidden="true"> / </span>
          </Fragment>
        ))}
      </nav>
      <div className="picker-input">
        <input
          autoFocus={focusSearch}
          value={query}
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          aria-label={searchLabel}
          placeholder={searchLabel}
          onChange={(event) => type(event.target.value)}
        />
      </div>

      <div
        className="picker-rows"
        ref={rowList}
        onScroll={(event) => {
          setKeyboardNavigation(false);
          onBrowse?.({
            query,
            chips,
            selected,
            scrollTop: event.currentTarget.scrollTop,
          });
        }}
      >
        {entries === null && <p className="picker-note dim">Reading models…</p>}
        {entries !== null && list_.length === 0 && (
          <p className="picker-note dim">
            {entries.length === 0
              ? "no connections read from this host"
              : "nothing matches"}
          </p>
        )}
        {list_.map((row, index) => (
          <Fragment key={rowKey(row)}>
            {sections[index] !== null &&
              (index === 0 || sections[index] !== sections[index - 1]) && (
                <h2 className="picker-section">{sections[index]}</h2>
              )}
            <ModelPickerRow
              row={row}
              chips={chips}
              openedAt={openedAt}
              selected={keyboardNavigation && index === selected}
              onPick={() => {
                choose(row);
                if (row.kind !== "connection" && row.kind !== "group")
                  onClose();
              }}
            />
          </Fragment>
        ))}
      </div>
    </div>
  );
}
