// The model picker (docs/web-client.md): one typeahead over the catalog tree,
// a popover anchored to the row on a wide window and a full-screen sheet on a
// phone. It is the terminal form's picker (docs/tui.md) rather than a second
// design — the same chips, the same rows and facts, the same keys where they
// map, and the same matching rule, which `matcher.ts` ports.
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

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  type CatalogEntry,
  fetchCatalog,
  fetchConnection,
} from "../api/sessionForm.ts";
import { type Chip, relative } from "./matcher.ts";
import {
  type Role,
  type Row,
  ageFact,
  catalogNodes,
  contextFact,
  descends,
  nameAdds,
  priceFact,
  rowSlug,
  rows,
} from "./pickerRows.ts";
import "./ModelPicker.css";

/** How stale a copy may be before a no-match query reads the connection again
 * (docs/tui.md). The host raises anything under 60 to 60 regardless. */
const MISS_MAX_AGE = 60;

export function ModelPicker({
  role,
  cwd,
  config,
  onPick,
  onClose,
}: {
  role: Role;
  cwd: string;
  config: string | null;
  /** A row was chosen: a model to set, or nothing for the typed-id row's
   * connection. */
  onPick: (selection: { connection: string; model: string }) => void;
  onClose: () => void;
}) {
  const [entries, setEntries] = useState<CatalogEntry[] | null>(null);
  const [query, setQuery] = useState("");
  const [chips, setChips] = useState<Chip[]>([]);
  const [selected, setSelected] = useState(0);
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
    fetchCatalog(cwd, config)
      .then((answer) => {
        if (!live) return;
        setEntries(answer.connections);
        // Then one awaited read per connection with no usable copy, each
        // replacing its own entry as it lands.
        for (const entry of answer.connections) {
          if (entry.status === "fresh" || entry.status === "unavailable") {
            continue;
          }
          fetchConnection(entry.connection, cwd, config)
            .then((fresh) => live && replace(entry.connection, fresh))
            .catch(() => {});
        }
      })
      .catch(() => live && setEntries([]));
    return () => {
      live = false;
    };
  }, [cwd, config, replace]);

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
      connection === null ||
      query.trim() === "" ||
      /\s/.test(query.trim()) ||
      refreshed.current.has(connection) ||
      list_.some((row) => row.kind !== "typed")
    ) {
      return;
    }
    refreshed.current.add(connection);
    fetchConnection(connection, cwd, config, MISS_MAX_AGE)
      .then((entry) => replace(connection, entry))
      .catch(() => {});
  }, [list_, chips, query, cwd, config, replace]);

  /** Set the query and return the selection to the first row (docs/tui.md): the
   * highlight is otherwise left over rows the new query does not have. Done at
   * the commit rather than in an effect, which is where the change happens. */
  const type = (next: string) => {
    setQuery(next);
    setSelected(0);
  };

  const choose = (row: Row | undefined) => {
    if (row === undefined) return;
    if (row.kind === "typed") {
      onPick({ connection: row.connection, model: row.model });
    } else if (row.kind === "model") {
      onPick({ connection: row.node.connection, model: row.node.model ?? "" });
    } else if (descends(row)) {
      const label =
        row.node.kind === "group" ? row.node.group : row.node.connection;
      setChips((current) => [...current, label ?? ""]);
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
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
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
      // Backspace on an empty query removes the last chip whole.
      event.preventDefault();
      setChips((current) => current.slice(0, -1));
    }
  };

  // The clock the age facts are computed against, read once when the picker
  // opens and held: an age that shifts on every render is not a fact, and
  // reading the clock during render is not allowed anyway. A picker left open
  // across a day shows a day-old age, which the next visit corrects.
  const [openedAt] = useState(() => Math.floor(Date.now() / 1000));

  return (
    <div className="picker" onKeyDown={onKey}>
      <div className="picker-input">
        {chips.map((chip, index) => (
          <span className="picker-chip" key={`${index}-${chip}`}>
            {chip}
            <button
              aria-label={`Leave ${chip}`}
              onClick={() =>
                setChips((current) => current.filter((_, i) => i !== index))
              }
            >
              ×
            </button>
          </span>
        ))}
        <input
          autoFocus
          value={query}
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          aria-label="Search models"
          placeholder={chips.length === 0 ? "search models" : ""}
          onChange={(event) => type(event.target.value)}
        />
      </div>

      <div className="picker-rows">
        {entries === null && <p className="picker-note dim">Reading models…</p>}
        {entries !== null && list_.length === 0 && (
          <p className="picker-note dim">
            {entries.length === 0
              ? "no connections read from this host"
              : "nothing matches"}
          </p>
        )}
        {list_.map((row, index) => (
          <PickerRow
            key={rowSlug(row)}
            row={row}
            chips={chips}
            openedAt={openedAt}
            selected={index === selected}
            onPick={() => {
              choose(row);
              if (row.kind !== "connection" && row.kind !== "group") onClose();
            }}
          />
        ))}
      </div>
    </div>
  );
}

/** One row: the id as the path shows it, the name when it adds something, and
 * up to three dim facts. A connection carries its provider and its state. */
function PickerRow({
  row,
  chips,
  openedAt,
  selected,
  onPick,
}: {
  row: Row;
  chips: Chip[];
  openedAt: number;
  selected: boolean;
  onPick: () => void;
}) {
  return (
    <button
      className={selected ? "picker-row selected" : "picker-row"}
      onClick={onPick}
      role="option"
      aria-selected={selected}
    >
      <span className="picker-cursor">❯</span>
      <span className="picker-text">
        <span className="picker-id">
          {row.kind === "typed" ? row.model : shownId(row, chips)}
        </span>
        {row.kind === "model" && nameAdds(row.node, row.model.name) && (
          <span className="picker-name dim">{row.model.name}</span>
        )}
      </span>
      {row.kind === "connection" ? (
        <span className="picker-fact dim">
          {row.entry.provider}
          {row.entry.status === "unavailable" ? " · unavailable" : ""}
          {row.entry.status === "failed" ? " · failed" : ""}
        </span>
      ) : row.kind === "group" ? (
        <span className="picker-fact dim">{row.count} models</span>
      ) : row.kind === "model" ? (
        // Age, price, context — the order they give way in when a row is
        // narrow (ModelPicker.css), and the columns they align in.
        <span className="picker-facts dim">
          <span>{ageFact(row.model, openedAt) ?? ""}</span>
          <span>{priceFact(row.model) ?? ""}</span>
          <span>{contextFact(row.model) ?? ""}</span>
        </span>
      ) : null}
      {row.kind === "model" && row.recent && (
        <span className="picker-fact dim">recent</span>
      )}
      {row.kind === "typed" && (
        <span className="picker-fact dim">use as model id</span>
      )}
    </button>
  );
}

/** The id as it reads under the chips: a model's own id under a connection, and
 * the author dropped under a group. */
const shownId = (row: Row, chips: Chip[]): string =>
  row.kind === "model" || row.kind === "group"
    ? relative(row.node, chips)
    : rowSlug(row);
