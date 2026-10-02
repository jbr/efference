// Location choice and optional recent-entry removal (docs/web-session-creation.md).
import type { ProjectEntry } from "../api/sessionForm.ts";
import { homeRelative } from "./identity.ts";
import { splitPath } from "./overview.ts";
import { showsConfig } from "./sessionForm.ts";

export function LocationRow({
  entry,
  projects,
  selected,
  editing,
  onSelect,
  onForget,
}: {
  entry: ProjectEntry;
  projects: ProjectEntry[];
  selected: boolean;
  editing: boolean;
  onSelect: () => void;
  onForget?: () => void;
}) {
  const [parent, name] = splitPath(homeRelative(entry.cwd));
  return (
    <div
      className={`location${selected ? " selected" : ""}${editing ? " editing" : ""}`}
    >
      {entry.config_key !== "" && onForget !== undefined && (
        <div
          className="location-forget-slot"
          inert={!editing}
          aria-hidden={!editing}
        >
          <button
            type="button"
            disabled={!editing}
            tabIndex={editing ? 0 : -1}
            className="location-forget"
            aria-label={`Remove ${entry.cwd}${entry.config === null ? "" : ` (${entry.config})`} from recent locations`}
            title="Remove from recent locations"
            onClick={onForget}
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
              <path d="M8 12h8" />
            </svg>
          </button>
        </div>
      )}
      <button type="button" className="location-pick" onClick={onSelect}>
        <span className="location-line">
          <span className="location-path">
            <span className="location-parent">{parent}</span>
            <span className="location-name">{name}</span>
          </span>
          <span aria-hidden="true" className="location-next">
            ›
          </span>
        </span>
        {showsConfig(projects, entry) && (
          <code className="location-config">
            {entry.config ?? "project config"}
          </code>
        )}
      </button>
    </div>
  );
}
