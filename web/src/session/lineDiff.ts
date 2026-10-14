// The viewer's one diff engine (docs/web-client.md): line-level,
// over successive fetches of the same file, so a highlight means "what changed
// since you last looked". One engine for every file type — markdown maps the
// same line ranges onto its blocks rather than diffing again in another
// domain.
//
// Hand-rolled Myers rather than a dependency: the inputs are two versions of
// one file an edit apart, so the edit distance is small in every case that
// matters, and the common-affix trim below usually leaves a few lines. The
// budget bail keeps a pathological pair (a wholesale rewrite) from burning
// quadratic time to say what "everything changed" says for free.

export interface LineChanges {
  /** 1-based inclusive ranges of new-text lines that are inserted or
   * modified. */
  changed: [number, number][];
  /** New-text line numbers *after which* old lines were deleted with nothing
   * inserted in their place — 0 for a deletion before the first line. A pure
   * deletion leaves no line to tint, so it is reported as a seam instead. */
  removedAt: number[];
}

export const NO_CHANGES: LineChanges = { changed: [], removedAt: [] };

/** Myers edit-distance budget. Past it the middle is reported as one changed
 * block — for a diff meant to draw the eye, "all of this changed" is the
 * honest summary of an edit that large. Overridable only by tests. */
const MAX_D = 1000;

type Op = "eq" | "del" | "ins";

/** Line ranges of `newText` changed relative to `oldText`. */
export function diffLines(
  oldText: string,
  newText: string,
  maxD: number = MAX_D,
): LineChanges {
  if (oldText === newText) return NO_CHANGES;
  const a = oldText.split("\n");
  const b = newText.split("\n");

  // Common affixes carry no information and dominate a single-edit save;
  // trimming them is what keeps Myers' input to roughly the edit itself.
  let prefix = 0;
  while (prefix < a.length && prefix < b.length && a[prefix] === b[prefix]) {
    prefix++;
  }
  let suffix = 0;
  while (
    suffix < a.length - prefix &&
    suffix < b.length - prefix &&
    a[a.length - 1 - suffix] === b[b.length - 1 - suffix]
  ) {
    suffix++;
  }
  const aMid = a.slice(prefix, a.length - suffix);
  const bMid = b.slice(prefix, b.length - suffix);

  if (bMid.length === 0 && aMid.length === 0) return NO_CHANGES;
  if (aMid.length === 0) {
    return { changed: [[prefix + 1, prefix + bMid.length]], removedAt: [] };
  }
  if (bMid.length === 0) {
    return { changed: [], removedAt: [prefix] };
  }

  const ops = myers(aMid, bMid, maxD);
  if (ops === null) {
    // Over budget: the whole middle as one changed block.
    return { changed: [[prefix + 1, prefix + bMid.length]], removedAt: [] };
  }
  return collect(ops, prefix);
}

/** Group the edit script into maximal non-`eq` runs; a run that inserted
 * anything is a changed range over its inserted new lines (deletions in the
 * same run are the "modified" half), a run of only deletions is a seam. */
function collect(ops: Op[], prefix: number): LineChanges {
  const changed: [number, number][] = [];
  const removedAt: number[] = [];
  let j = prefix; // new-text lines emitted so far
  let i = 0;
  while (i < ops.length) {
    if (ops[i] === "eq") {
      j++;
      i++;
      continue;
    }
    const start = j;
    let inserted = 0;
    while (i < ops.length && ops[i] !== "eq") {
      if (ops[i] === "ins") {
        inserted++;
        j++;
      }
      i++;
    }
    if (inserted > 0) changed.push([start + 1, start + inserted]);
    else removedAt.push(start);
  }
  return { changed, removedAt };
}

/** Classic O(ND) greedy Myers with a full trace for backtracking; `null`
 * when the edit distance exceeds the budget. */
function myers(a: string[], b: string[], maxD: number): Op[] | null {
  const n = a.length;
  const m = b.length;
  const max = Math.min(n + m, maxD);
  const offset = max;
  const v = new Int32Array(2 * max + 1);
  const trace: Int32Array[] = [];
  for (let d = 0; d <= max; d++) {
    trace.push(v.slice());
    for (let k = -d; k <= d; k += 2) {
      if (k < -max || k > max) continue;
      let x =
        k === -d || (k !== d && v[offset + k - 1] < v[offset + k + 1])
          ? v[offset + k + 1]
          : v[offset + k - 1] + 1;
      let y = x - k;
      while (x < n && y < m && a[x] === b[y]) {
        x++;
        y++;
      }
      v[offset + k] = x;
      if (x >= n && y >= m) return backtrack(trace, d, n, m, offset);
    }
  }
  return null;
}

function backtrack(
  trace: Int32Array[],
  d: number,
  n: number,
  m: number,
  offset: number,
): Op[] {
  const ops: Op[] = [];
  let x = n;
  let y = m;
  for (; d > 0; d--) {
    const v = trace[d];
    const k = x - y;
    const fromDown =
      k === -d || (k !== d && v[offset + k - 1] < v[offset + k + 1]);
    const prevK = fromDown ? k + 1 : k - 1;
    const prevX = v[offset + prevK];
    const prevY = prevX - prevK;
    while (x > prevX && y > prevY) {
      ops.push("eq");
      x--;
      y--;
    }
    if (fromDown) {
      ops.push("ins");
      y--;
    } else {
      ops.push("del");
      x--;
    }
  }
  while (x > 0 && y > 0) {
    ops.push("eq");
    x--;
    y--;
  }
  while (x > 0) {
    ops.push("del");
    x--;
  }
  while (y > 0) {
    ops.push("ins");
    y--;
  }
  return ops.reverse();
}
