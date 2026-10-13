import { RangeContent } from "../ui/RangeContent.tsx";
import { Specimen } from "./Specimens.tsx";
const code = Array.from({ length: 64 }, (_, i) => {
  const n = i + 1;
  if (n === 8)
    return "/* Context begins here; highlighting uses the whole captured source.";
  if (n === 17) return " */";
  if (n > 8 && n < 17) return ` * Comment line ${n}, initially omitted.`;
  return n % 8 === 0 ? "" : `const value${n} = inspectLine(${n});`;
}).join("\n");
const plain = Array.from(
  { length: 45 },
  (_, i) =>
    `Line ${i + 1}: captured output, with adjacent context available to reveal.`,
).join("\n");
export function RangeContentSpecimens() {
  return (
    <>
      <Specimen name="Captured source · gutter marks retained lines · expand either edge of a gap">
        <RangeContent
          text={code}
          retained={[
            [3, 6],
            [13, 20],
            [48, 52],
          ]}
          lang="typescript"
        />
      </Specimen>
      <Specimen name="Plain content · same interaction without highlighting">
        <RangeContent
          text={plain}
          retained={[
            [1, 4],
            [30, 34],
          ]}
        />
      </Specimen>
    </>
  );
}
