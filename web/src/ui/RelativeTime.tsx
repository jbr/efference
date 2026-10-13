import "@github/relative-time-element";

export function RelativeTime({
  date,
  className,
  tense = "past",
}: {
  date: Date | number;
  className?: string;
  tense?: "past" | "future" | "auto";
}) {
  const d = typeof date === "number" ? new Date(date) : date;
  const iso = d.toISOString();

  return <relative-time datetime={iso} class={className} tense={tense} />;
}
