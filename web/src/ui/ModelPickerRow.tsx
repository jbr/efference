// Model choice and information are separate controls (docs/web-session-creation.md):
// expanding details must not choose a model.
import { useId, useState } from "react";
import { type Chip, relative } from "./matcher.ts";
import { type Row, ageFact, descends, rowSlug } from "./pickerRows.ts";

export function ModelPickerRow({
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
  const shownId =
    row.kind === "model"
      ? relative(
          row.node,
          chips.length === 0
            ? [
                row.node.connection,
                ...(row.node.group === undefined ? [] : [row.node.group]),
              ]
            : chips,
        )
      : row.kind === "group"
        ? relative(row.node, chips)
        : rowSlug(row);
  const name = row.kind === "model" ? row.model.name?.trim() : undefined;
  return (
    <div className="picker-candidate">
      <button
        className={`picker-row selection-row${selected ? " selected" : ""}${descends(row) ? " picker-folder" : " picker-model"}`}
        onClick={onPick}
        role="option"
        aria-selected={selected}
      >
        <span className="picker-text">
          <span className="picker-label">
            {row.kind === "model" && chips.length === 0 && (
              <>
                <span className="picker-origin">
                  {row.node.connection}
                </span>{" "}
              </>
            )}
            {name ? (
              <span className="picker-name">{name}</span>
            ) : (
              <span className="picker-id">
                {row.kind === "typed" ? row.model : shownId}
              </span>
            )}
          </span>
        </span>
        {row.kind === "connection" ? (
          <span className="picker-fact dim">
            {row.entry.models.length}{" "}
            {row.entry.models.length === 1 ? "model" : "models"}
            {row.entry.status === "unavailable" ? " · unavailable" : ""}
            {row.entry.status === "failed" ? " · failed" : ""}
          </span>
        ) : row.kind === "group" ? (
          <span className="picker-fact dim">
            {row.count} {row.count === 1 ? "model" : "models"}
          </span>
        ) : null}
        {row.kind === "typed" && (
          <span className="picker-fact dim">use as model id</span>
        )}
        {descends(row) && (
          <span className="picker-next" aria-hidden="true">
            ›
          </span>
        )}
      </button>
      {row.kind === "model" && <ModelDetails row={row} openedAt={openedAt} />}
    </div>
  );
}

function ModelDetails({
  row,
  openedAt,
}: {
  row: Extract<Row, { kind: "model" }>;
  openedAt: number;
}) {
  const [expanded, setExpanded] = useState(false);
  const detailsId = useId();
  const description = row.model.description?.trim();
  const { context, price, reasoning, images } = row.model;
  const facts = [
    { label: "Full identifier", value: rowSlug(row) },
    { label: "Author", value: row.node.group ?? null },
    {
      label: "Context window",
      value:
        context !== undefined && context > 0
          ? `${context.toLocaleString()} tokens`
          : null,
    },
    {
      label: "Input price / 1M tokens",
      value: price ? `$${price.input}` : null,
    },
    {
      label: "Output price / 1M tokens",
      value: price ? `$${price.output}` : null,
    },
    { label: "Model age", value: ageFact(row.model, openedAt) },
    {
      label: "Reasoning",
      value:
        reasoning === undefined
          ? null
          : reasoning
            ? "Supported"
            : "Not supported",
    },
    {
      label: "Image input",
      value:
        images === undefined ? null : images ? "Supported" : "Not supported",
    },
    { label: "Recent pick", value: row.recent ? "Yes" : null },
  ].filter((fact) => fact.value !== null);
  return (
    <>
      <button
        className="picker-details-toggle"
        aria-label={`About ${rowSlug(row)}`}
        title="About this model"
        aria-expanded={expanded}
        aria-controls={detailsId}
        onClick={() => setExpanded((value) => !value)}
      >
        <svg
          width="18"
          height="18"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.5"
          aria-hidden="true"
          focusable="false"
        >
          <circle cx="12" cy="12" r="9" />
          <path d="M12 11v6" />
          <circle cx="12" cy="7.5" r="1" fill="currentColor" stroke="none" />
        </svg>
      </button>
      <div
        className="picker-details"
        id={detailsId}
        hidden={!expanded}
        role="region"
        aria-label={`Details for ${rowSlug(row)}`}
      >
        {facts.length > 0 && (
          <dl className="picker-parameters">
            {facts.map((fact) => (
              <div key={fact.label}>
                <dt>{fact.label}</dt>
                <dd>{fact.value}</dd>
              </div>
            ))}
          </dl>
        )}
        {description && (
          <div className="picker-description">
            <p className="picker-description-source">Provider description</p>
            <p>{description}</p>
          </div>
        )}
      </div>
    </>
  );
}
