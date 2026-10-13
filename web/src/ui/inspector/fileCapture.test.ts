import { expect, test } from "vitest";
import { capturedReads, currentReadPath } from "./fileCapture.ts";
test("maps raw-result curation positions into each captured file", () => {
  const raw =
    "==t BEGIN a.rs, LENGTH: 10 t==\none\ntwo\nthree\n==t END a.rs t==\n==t BEGIN b.txt, LENGTH: 10 t==\nfour\nfive\n==t END b.txt t==";
  const files = capturedReads(raw, {
    Distilled: {
      keep: {
        Ranges: [
          [3, 3],
          [8, 8],
        ],
      },
      note: null,
    },
  });
  expect(
    files.map(({ path, text, retained }) => ({ path, text, retained })),
  ).toEqual([
    { path: "a.rs", text: "one\ntwo\nthree", retained: [[2, 2]] },
    { path: "b.txt", text: "four\nfive", retained: [[2, 2]] },
  ]);
  expect(
    capturedReads(raw, "Elided").every((file) => file.retained.length === 0),
  ).toBe(true);
});
test("does not mislabel error output as captured file contents", () => {
  expect(
    capturedReads(
      "==t BEGIN ERROR missing.rs t==\nnot found\n==t END ERROR missing.rs t==",
      "Raw",
    ),
  ).toEqual([]);
});

test("ranged read prefixes become file coordinates, not duplicated code text", () => {
  const [file] = capturedReads(
    "==t BEGIN /root/src/a.rs, LENGTH: 200, LINES: 40-42 t==\n40\tfirst\n41\tsecond\n42\tthird\n==t END /root/src/a.rs t==",
    { Distilled: { keep: { Ranges: [[3, 3]] }, note: null } },
  );
  expect(file.text).toBe("first\nsecond\nthird");
  expect(file.startLine).toBe(40);
  expect(file.retained).toEqual([[2, 2]]);
  expect(currentReadPath(file, { paths: ["src/a.rs:40-42"] })).toBe("src/a.rs");
  expect(currentReadPath(file, { paths: ["unrelated.rs:40-42"] })).toBeNull();
  expect(
    currentReadPath(
      { ...file, path: "/root/src/name:40" },
      { paths: ["src/name:40"] },
    ),
  ).toBe("src/name:40");
});
