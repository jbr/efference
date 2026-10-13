import { useEffect, useState } from "react";
import type { Process } from "../api/types.ts";
import "./TaskStrip.css";

export function TaskStrip({
  processes,
  selected,
  onInspect,
}: {
  processes: Process[];
  selected: string | null;
  onInspect: (id: string) => void;
}) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 250);
    return () => clearInterval(timer);
  }, []);
  const visible = processes.filter(
    (p) => p.call_id != null && (p.background || now - p.spawned_at >= 1000),
  );
  if (visible.length === 0) return null;
  return (
    <div className="task-strip" aria-label="Running tasks">
      <span className="dim">Running</span>
      <div className="task-strip-items">
        {visible.map((p) => (
          <button
            key={p.id}
            aria-pressed={selected === p.call_id}
            onClick={() => onInspect(p.call_id!)}
            title={p.command}
          >
            <span>{p.command.trim().split(/\s+/)[0]}</span>
            <span className="dim">
              {Math.max(0, Math.floor((now - p.spawned_at) / 1000))}s
            </span>
          </button>
        ))}
      </div>
    </div>
  );
}
