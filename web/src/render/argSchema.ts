// Destructuring a call's arguments against the tool's declared schema, for the
// inspector's args tab (docs/web-client.md "The inspector"). Pure: a schema and
// the already-parsed sent fields in, rows out — the fetch (ui/toolDefinition.ts)
// and the rendering (ui/Inspector.tsx) sit on either side of it.
//
// The two mismatches are the point. A key the schema does not declare is the
// `crate` vs `crate_name` failure shape, which today shows as an unremarkable
// argument; a declared key the call omitted — a `required` one especially — is
// the shape of a whole class of failed calls, and nothing surfaces it at all.

import type { ArgField } from "./parse.ts";

/** The subset of JSON Schema this reads: the object's properties, their prose,
 * and which of them are required. Anything else in the schema is carried by the
 * raw definition and not consulted here. */
export interface SchemaProperty {
  key: string;
  description: string | null;
  required: boolean;
}

/** One argument the call sent, glossed by the schema when it declares it. */
export interface SentArg {
  field: ArgField;
  /** The schema's per-property description, else null (undeclared, or a
   * property with no prose of its own). */
  description: string | null;
  required: boolean;
  /** The schema declares no such property — flagged, because the call was
   * judged against exactly this schema. Always false with no definition. */
  undeclared: boolean;
}

/** A property the schema declares that the call did not pass. */
export interface AbsentArg {
  key: string;
  description: string | null;
  required: boolean;
}

export interface ArgsAgainstSchema {
  sent: SentArg[];
  absent: AbsentArg[];
}

/** `intent` is the harness's injected property (docs/tools.md), and it is
 * already the call's leading line above the argument list, which is why
 * `callArgs` filters it from the sent side. It is filtered from the declared
 * side for the same reason: a toolbox tool's served schema always declares it,
 * so leaving it in would report "declared but absent" on every call that
 * omitted it and gloss a line the tab renders elsewhere on every call that did
 * not. Neither half of the analysis sees it. */
const INJECTED = "intent";

/** The declared properties of an `input_schema`, in declaration order — or
 * null when there is no object schema to read. Null and `[]` are different
 * answers: a tool that declares no properties makes every sent key undeclared,
 * while an unreadable schema makes no claim about any of them. */
export function schemaProperties(schema: unknown): SchemaProperty[] | null {
  if (typeof schema !== "object" || schema === null) return null;
  const obj = schema as Record<string, unknown>;
  const properties = obj.properties;
  if (typeof properties !== "object" || properties === null) return null;
  const required = new Set(
    Array.isArray(obj.required)
      ? obj.required.filter((k): k is string => typeof k === "string")
      : [],
  );
  return Object.entries(properties as Record<string, unknown>)
    .filter(([key]) => key !== INJECTED)
    .map(([key, value]) => ({
      key,
      description:
        typeof value === "object" &&
        value !== null &&
        typeof (value as Record<string, unknown>).description === "string"
          ? ((value as Record<string, unknown>).description as string)
          : null,
      required: required.has(key),
    }));
}

/**
 * The sent arguments interleaved with what the schema declares.
 *
 * `schema` is null when there is no definition to have — a session read with no
 * daemon behind it, a `404`, a failed fetch. That case is not an error state:
 * every sent field comes back unglossed and unflagged with an empty `absent`,
 * which renders exactly as the tab did before schemas were fetched at all.
 */
export function argsAgainstSchema(
  fields: ArgField[],
  schema: unknown,
): ArgsAgainstSchema {
  const read = schemaProperties(schema);
  const declared = read ?? [];
  const byKey = new Map(declared.map((p) => [p.key, p]));
  const sentKeys = new Set(fields.map((f) => f.key));
  const sent = fields.map((field): SentArg => {
    const property = byKey.get(field.key);
    if (property === undefined) {
      return {
        field,
        description: null,
        required: false,
        // With no readable schema nothing can be called undeclared: silence is
        // absence of knowledge, not evidence the key is wrong.
        undeclared: read !== null,
      };
    }
    return {
      field,
      description: property.description,
      required: property.required,
      undeclared: false,
    };
  });
  const absent = declared
    .filter((p) => !sentKeys.has(p.key))
    .map(({ key, description, required }): AbsentArg => ({
      key,
      description,
      required,
    }));
  return { sent, absent };
}
