import "@github/relative-time-element";

export function RelativeTime({
  date,
  className,
}: {
  date: Date | number;
  className?: string;
}) {
  const d = typeof date === "number" ? new Date(date) : date;
  const iso = d.toISOString();

  return <relative-time datetime={iso} class={className} tense="past" />;
}
