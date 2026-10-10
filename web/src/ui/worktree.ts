// The notice a `Worktree` entry carries (docs/worktrees.md): one line per
// event, in the shape the transcript already uses for `Resumed`. The line is
// composed in the component (`Transcript.tsx`), where the branch renders as a
// code chip like the rest of the UI — this module holds the pieces that stay
// plain text, and the output cleaning.

/** How a checkout's exit status reads on the notice line: nothing when the
 * setup succeeded, the status otherwise. A nonzero status is stated on the
 * line itself, since a failed setup is the reason the entry exists. */
export function setupNote(exitCode: number | null): string {
  if (exitCode === 0) return "";
  if (exitCode === null) return " (setup failed: git ended by signal)";
  return ` (setup failed: exit ${exitCode})`;
}

/** Git's progress meters (`Updating files: 96% …\r`) redraw one line in place;
 * kept verbatim they are most of what a checkout prints. The host cleans this
 * at capture (`without_overwrites`, src/worktree.rs); old entries still hold
 * the raw redraws, so the renderer applies the same last-segment-per-line rule
 * again — a no-op on already-clean output. */
export function cleanWorktreeOutput(output: string): string {
  return output
    .split("\n")
    .map((segment) => {
      const trimmed = segment.endsWith("\r") ? segment.slice(0, -1) : segment;
      return trimmed.split("\r").pop() ?? "";
    })
    .join("\n");
}
