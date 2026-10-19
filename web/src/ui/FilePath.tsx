// Where the file sidebar is: the working directory's name, then each directory
// down to the open file (docs/web-client.md "The file viewer"). Every segment
// but the last returns to the tree at that directory, which is how a reader
// gets from a file back to the listing.

export function FilePath({
  root,
  path,
  onDir,
}: {
  /** The working directory's own name — the tree's root. */
  root: string;
  /** The open file, relative to the root; `null` on the tree itself. */
  path: string | null;
  /** Show the tree at a directory; `""` is the root. */
  onDir: (dir: string) => void;
}) {
  const segments = path === null ? [] : path.split("/");
  return (
    <nav className="file-path" aria-label="file path">
      {segments.length === 0 ? (
        <strong>{root}</strong>
      ) : (
        <button className="file-path-dir" onClick={() => onDir("")}>
          {root}
        </button>
      )}
      {segments.map((name, i) => {
        const last = i === segments.length - 1;
        return (
          <span key={i} className="file-path-step">
            <span className="file-path-sep" aria-hidden="true">
              /
            </span>
            {last ? (
              <strong aria-current="page">{name}</strong>
            ) : (
              <button
                className="file-path-dir"
                onClick={() => onDir(segments.slice(0, i + 1).join("/"))}
              >
                {name}
              </button>
            )}
          </span>
        );
      })}
    </nav>
  );
}
