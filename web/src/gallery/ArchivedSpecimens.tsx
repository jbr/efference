import type { ArchivedSession } from "../api/types.ts";
import { ArchivedCard } from "../ui/ArchivedCard.tsx";
import { ArchivedList } from "../ui/ArchivedList.tsx";
import { Specimen } from "./Specimens.tsx";

// A session the host lists as archived (docs/resumption.md): the row resumes it,
// and the menu beside it opens the model switch, which resumes it on the chosen
// models instead. Fixture state only — the listing itself is a subscription.

const session = (
  id: string,
  title: string | null,
  minutesAgo: number,
): ArchivedSession => ({
  id,
  cwd: "/Users/jbr/code/efference",
  title: title ?? undefined,
  last_active_at: Date.now() - minutesAgo * 60_000,
  state: "archived",
});

export function ArchivedSpecimens() {
  return (
    <>
      <Specimen name="archived card — the title resumes, the menu switches">
        <div className="overview g-overview g-archived">
          <div className="card-grid">
            <ArchivedCard
              session={session(
                "quick-sparrow",
                "Implementing worktree UI in the TUI",
                48,
              )}
              resuming={false}
              onResume={() => {}}
              onSwitch={() => {}}
            />
          </div>
        </div>
      </Specimen>
      <Specimen name="recent band — a row per session, a menu per row">
        <div className="overview g-overview g-archived">
          <ArchivedList
            sessions={[
              session(
                "quick-sparrow",
                "Implementing worktree UI in the TUI",
                48,
              ),
              session("lucid-plover", null, 1_440),
              session("fold-probe", "Probing context folding behavior", 2_880),
            ]}
            remainderCount={3}
            resuming={null}
            onResume={() => {}}
            onSwitch={() => {}}
            onOpenPicker={() => {}}
          />
        </div>
      </Specimen>
    </>
  );
}
