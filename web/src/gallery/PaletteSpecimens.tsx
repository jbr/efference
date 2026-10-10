import type { CSSProperties } from "react";
import { Code } from "../ui/Code.tsx";
import { CallBlock } from "../ui/ToolBlocks.tsx";

const palettes = [
  { name: "Original mint", dark: "#9dc0b0", light: "#47705c" },
  { name: "Silver lilac", dark: "#bfb0d5", light: "#765d90" },
  { name: "Misty blue · applied draft", dark: "#aebad0", light: "#566781" },
  { name: "Dusty rose", dark: "#c8a8b8", light: "#875c72" },
];
const source = `from pathlib import Path
path = Path("docs/web-client.md")
label = "Context window"
levels = [10, 50, 75, 90, 100]
print(f"{label}: {path}")`;

export function PaletteSpecimens() {
  return (
    <>
      <p className="g-intro">
        Compare shell strings, paths, and heredoc bodies. Other syntax and diff
        colors stay unchanged. Misty blue is applied in the draft; the other
        specimens are local alternatives. Python string highlighting is tracked
        separately in the backlog.
      </p>
      <div className="g-palettes">
        {palettes.map((palette) => (
          <div
            key={palette.name}
            className="g-palette g-example"
            style={
              {
                "--palette-dark": palette.dark,
                "--palette-light": palette.light,
              } as CSSProperties
            }
          >
            <h3>{palette.name}</h3>
            <div className="transcript">
              <div className="call">
                <CallBlock
                  call={{
                    id: `palette-${palette.name}`,
                    name: "bash",
                    input: {
                      command:
                        "python3 -c \"print('Context window')\" docs/web-client.md",
                    },
                  }}
                />
              </div>
            </div>
            <Code text={`python3 - <<'PY'\n${source}\nPY`} lang="bash" />
            <p className="g-caption">
              After Dark {palette.dark} · Lilac {palette.light}
            </p>
          </div>
        ))}
      </div>
    </>
  );
}
