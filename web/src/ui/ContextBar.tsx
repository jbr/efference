import type { CSSProperties } from "react";

/** Context fill shared by header and hover card (docs/web-client.md). */
export function ContextBar({
  used,
  limit,
}: {
  used: number | null;
  limit: number;
}) {
  const ratio =
    used === null || limit <= 0 ? 0 : Math.max(0, Math.min(1, used / limit));
  const style: CSSProperties &
    Record<"--light-level" | "--meter-warm" | "--meter-hot", string | number> =
    {
      width: `${ratio * 100}%`,
      "--light-level": ratio,
      "--meter-warm": Math.max(0, Math.min(1, (ratio - 0.5) / 0.35)),
      "--meter-hot": Math.max(0, Math.min(1, (ratio - 0.85) / 0.15)),
    };
  return (
    <span
      className="meter"
      aria-hidden="true"
      data-empty={ratio === 0 ? "" : undefined}
    >
      <span className="meter-fill" style={style} />
    </span>
  );
}
