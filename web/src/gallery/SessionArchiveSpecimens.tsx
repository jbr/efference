import { useState } from "react";
import { HoverCard } from "../ui/SessionHoverCard.tsx";
import type { CardModel } from "../ui/overview.ts";
import { tabCards } from "./fixtures.ts";
import { Specimen } from "./Specimens.tsx";

function ArchivePreview({
  card,
  fail = false,
}: {
  card: CardModel;
  fail?: boolean;
}) {
  const [archived, setArchived] = useState(false);
  return archived ? (
    <div>
      <p>{card.id} archived (gallery only).</p>
      <button onClick={() => setArchived(false)}>Reset</button>
    </div>
  ) : (
    <div className="switcher g-switcher g-pop-host">
      <HoverCard
        card={card}
        onArchive={async () => {
          await new Promise((resolve) => setTimeout(resolve, 800));
          if (fail) throw new Error("The host is unavailable. Try again.");
          setArchived(true);
        }}
      />
    </div>
  );
}

export function SessionArchiveSpecimens() {
  return (
    <>
      <Specimen name="idle session — archive directly (local demo)">
        <ArchivePreview card={tabCards[1]} />
      </Specimen>
      <Specimen name="active session — confirm archive (local demo)">
        <ArchivePreview card={tabCards[0]} />
      </Specimen>
      <Specimen name="archive failure — keeps details and allows retry (local demo)">
        <ArchivePreview card={tabCards[1]} fail />
      </Specimen>
    </>
  );
}
