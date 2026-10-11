// Bounded recorder storage; evicted frames advance the replay base (docs/web-client.md).
import type { Frame } from "../api/types.ts";

export class FrameTape {
  private frames: Frame[] = [];
  private head = 0;

  constructor(private readonly capacity: number) {
    if (!Number.isInteger(capacity) || capacity < 1) {
      throw new Error("frame tape capacity must be a positive integer");
    }
  }

  get length(): number {
    return this.frames.length;
  }

  at(index: number): Frame {
    return this.frames[(this.head + index) % this.length];
  }

  push(frame: Frame): Frame | undefined {
    if (this.length < this.capacity) {
      this.frames.push(frame);
      return;
    }
    const evicted = this.frames[this.head];
    this.frames[this.head] = frame;
    this.head = (this.head + 1) % this.capacity;
    return evicted;
  }

  snapshot(end = this.length): Frame[] {
    return Array.from({ length: end }, (_, index) => this.at(index));
  }
}
