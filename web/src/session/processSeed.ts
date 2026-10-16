// The tasks pane's cold seed. The `output` durability class is ephemeral and
// lossy with no replay (docs/control-api.md), so a process that was already
// running when the client connected — or that has simply been quiet, which is
// usually why it was backgrounded — has no deltas to build a tail from and the
// pane renders an empty <pre>. `GET /processes/:id/output` is the seed the
// daemon provides for exactly this (docs/process-monitor.md); the TUI does the same
// thing from its bridge.

import { fetchProcessOutput } from "../api/http.ts";
import type { Process } from "../api/types.ts";
import { seedProcessOutput, type Store } from "./store.ts";

export class ProcessSeeder {
  private seeded = new Set<number>();

  constructor(private store: Store) {}

  /**
   * Seed any process in the topic we have not read yet. An id with output
   * already buffered is skipped: the retained read is a snapshot that would
   * contain those same bytes, so seeding a warm buffer duplicates them. That
   * leaves exactly the cold case, where the buffer is empty and everything
   * retained belongs in front of whatever arrives next.
   */
  sync(processes: Process[]): void {
    const buffered = this.store.snapshot().streaming.processOutput;
    for (const proc of processes) {
      if (this.seeded.has(proc.id)) continue;
      this.seeded.add(proc.id);
      if (buffered[proc.id] !== undefined) continue;
      void fetchProcessOutput(proc.id)
        .then((read) =>
          this.store.update((s) =>
            seedProcessOutput(s, proc.id, read.output, read.lost),
          ),
        )
        // A seed that fails costs the tail its history, not its future: the
        // deltas still land. Nothing to surface.
        .catch(() => {});
    }
  }
}
