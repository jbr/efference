import { expect, test } from "vitest";
import { callArgs } from "./parse.ts";
import { argsAgainstSchema, schemaProperties } from "./argSchema.ts";

/** A schema shaped like one the route serves: per-property descriptions,
 * `required`, and the injected `intent` a toolbox tool always carries. */
const ferritinGet = {
  type: "object",
  properties: {
    intent: { type: "string", description: "why this call is being made" },
    path: {
      type: "string",
      description: "Item path, e.g. `serde::Deserialize`.",
    },
    crate_name: { type: "string", description: "The crate to look in." },
    private: { type: "boolean" },
  },
  required: ["intent", "path"],
};

test("each sent argument is glossed by the schema, and required is marked", () => {
  const fields = callArgs("ferritin__get", {
    intent: "check the trait",
    path: "serde::Deserialize",
  });
  const { sent, absent } = argsAgainstSchema(fields, ferritinGet);

  expect(sent.map((a) => a.field.key)).toEqual(["path"]);
  expect(sent[0].description).toBe("Item path, e.g. `serde::Deserialize`.");
  expect(sent[0].required).toBe(true);
  expect(sent[0].undeclared).toBe(false);
  // The value itself is untouched — it is still what `callArgs` produced.
  expect(sent[0].field.scalar).toBe("serde::Deserialize");
  // A declared property with no prose glosses to nothing rather than to "".
  expect(absent.find((a) => a.key === "private")?.description).toBe(null);
});

test("a key the schema does not declare is flagged", () => {
  // The `crate` vs `crate_name` failure shape: today this renders as an
  // unremarkable argument and the call fails somewhere downstream.
  const fields = callArgs("ferritin__get", { path: "Vec", crate: "std" });
  const { sent } = argsAgainstSchema(fields, ferritinGet);

  const crate = sent.find((a) => a.field.key === "crate");
  expect(crate?.undeclared).toBe(true);
  expect(sent.find((a) => a.field.key === "path")?.undeclared).toBe(false);
});

test("declared properties the call omitted are listed, required ones marked", () => {
  const fields = callArgs("ferritin__get", { crate_name: "std" });
  const { absent } = argsAgainstSchema(fields, ferritinGet);

  expect(absent.map((a) => a.key)).toEqual(["path", "private"]);
  expect(absent.find((a) => a.key === "path")?.required).toBe(true);
  expect(absent.find((a) => a.key === "private")?.required).toBe(false);
});

test("intent is neither glossed nor reported absent", () => {
  // The schema served is the one the model saw, so a toolbox tool always
  // declares `intent`; the tab renders it as the call's leading line, on both
  // the sent side (`callArgs` filters it) and the declared side.
  expect((schemaProperties(ferritinGet) ?? []).map((p) => p.key)).not.toContain(
    "intent",
  );

  const withIntent = argsAgainstSchema(
    callArgs("ferritin__get", { intent: "x", path: "Vec" }),
    ferritinGet,
  );
  expect(withIntent.sent.map((a) => a.field.key)).not.toContain("intent");
  expect(withIntent.absent.map((a) => a.key)).not.toContain("intent");

  // And a call that sent no intent at all does not report one missing, even
  // though the schema lists it as required.
  const without = argsAgainstSchema(
    callArgs("ferritin__get", { path: "Vec" }),
    ferritinGet,
  );
  expect(without.absent.map((a) => a.key)).not.toContain("intent");
});

test("with no definition the rows are exactly the sent fields, unflagged", () => {
  const fields = callArgs("ferritin__get", { path: "Vec", crate: "std" });
  const { sent, absent } = argsAgainstSchema(fields, null);

  expect(sent.map((a) => a.field)).toEqual(fields);
  expect(sent.every((a) => !a.undeclared && !a.required)).toBe(true);
  expect(sent.every((a) => a.description === null)).toBe(true);
  expect(absent).toEqual([]);
});

test("a schema with no properties object makes no claim about any key", () => {
  // Silence is absence of knowledge: an unreadable schema must not flag every
  // argument as undeclared.
  expect(schemaProperties({ type: "object" })).toBe(null);
  expect(schemaProperties("not a schema")).toBe(null);
  const { sent } = argsAgainstSchema(callArgs("t", { a: 1 }), {
    type: "object",
  });
  expect(sent[0].undeclared).toBe(false);

  // A tool that genuinely declares no properties is a different answer: there
  // the sent key really is undeclared.
  const declared = argsAgainstSchema(callArgs("t", { a: 1 }), {
    type: "object",
    properties: {},
  });
  expect(declared.sent[0].undeclared).toBe(true);
});
