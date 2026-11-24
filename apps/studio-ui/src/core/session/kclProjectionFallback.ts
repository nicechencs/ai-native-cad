/**
 * LOCAL VIEW-PROJECTION FALLBACK ONLY — NOT AN AUTHORITATIVE CAD WRITE PATH.
 *
 * When the Studio UI has no active backend connection (`cad-cli` / Live-Attach),
 * this module provides best-effort regex projection of parameter edits into the
 * local KCL text buffer and extracts numeric literals from KCL declarations back
 * into `CanonicalGraph` parameters.
 *
 * Authoritative CAD writes MUST be translated to `CadApiSemanticCommand[]` via
 * `cadApiAdapter.ts` and dispatched to Live-Attach / `cad-cli api apply`.
 * Keep all KCL source regex splicing isolated inside this fallback module so
 * `engineeringSession.ts` stays free of source-span regex manipulation.
 */
import type { CanonicalGraph, ParameterDef, Quantity } from "../../types/graphIr.ts";

/**
 * Best-effort local fallback that rewrites or inserts a parameter declaration in
 * a KCL text buffer when no backend semantic compiler is connected.
 */
export function projectParameterIntoKcl(source: string, parameter: ParameterDef): string {
  const literal = formatParameterLiteral(parameter);
  const escaped = escapeRegExp(parameter.name);
  const declaration = new RegExp(
    `(^|[^A-Za-z0-9_])(${escaped})(\\s*(?::[^\\n=]+)?=\\s*)([^\\n,]*)`,
  );
  if (!declaration.test(source)) {
    const line = `const ${parameter.name} = ${literal}`;
    const partAt = source.search(/^[ \t]*part\b/m);
    if (partAt >= 0) return `${source.slice(0, partAt)}${line}\n${source.slice(partAt)}`;
    return `${source.replace(/\s*$/, "")}\n${line}\n`;
  }
  return source.replace(declaration, `$1$2$3${literal}`);
}

/**
 * Best-effort local fallback that syncs numeric parameter literals found in KCL
 * text back into `graph.parameters` when editing raw KCL offline.
 */
export function syncGraphParametersFromKcl(graph: CanonicalGraph, source: string): CanonicalGraph {
  let parameters = graph.parameters;
  let changed = false;
  for (const param of Object.values(graph.parameters)) {
    const parsed = readDeclaredNumber(source, param.name);
    if (!parsed) continue;
    const next = parameterWithNumber(param, parsed.value, parsed.unit);
    if (JSON.stringify(next) === JSON.stringify(param)) continue;
    parameters = { ...(changed ? parameters : graph.parameters), [param.name]: next };
    changed = true;
  }
  return changed ? { ...graph, parameters } : graph;
}

function readDeclaredNumber(
  source: string,
  name: string,
): { readonly value: number; readonly unit: string | null } | null {
  const match = new RegExp(
    `(?:^|[^A-Za-z0-9_])${escapeRegExp(name)}\\s*(?::[^\\n=]+)?=\\s*([+-]?(?:\\d[\\d_]*|\\.\\d[\\d_]*)(?:\\.\\d[\\d_]*)?(?:[eE][+-]?\\d+)?)\\s*([A-Za-z][A-Za-z0-9*/]*)?`,
  ).exec(source);
  if (!match?.[1]) return null;
  const value = Number(match[1].replace(/_/g, ""));
  if (!Number.isFinite(value)) return null;
  return { value, unit: match[2] ?? null };
}

function parameterWithNumber(param: ParameterDef, value: number, unit: string | null): ParameterDef {
  const current = param.value;
  if (current.type === "quantity") {
    const next = withQuantityNumber(current.value, value, unit);
    return next === current.value ? param : { ...param, value: { type: "quantity", value: next } };
  }
  if (current.type === "int") {
    const next = Number.isInteger(value) ? value : Math.round(value);
    if (current.value === next) return param;
    return { ...param, value: { type: "int", value: next } };
  }
  if (current.type === "scalar") {
    if (current.value === value) return param;
    return { ...param, value: { type: "scalar", value } };
  }
  return param;
}

function withQuantityNumber(quantity: Quantity, value: number, unit: string | null): Quantity {
  switch (quantity.dimension) {
    case "density":
      return quantity.value === value ? quantity : { dimension: "density", value };
    case "length": {
      const nextUnit = unit === "mm" || unit === "m" || unit === "in" || unit === "um" || unit === "cm" ? unit : quantity.unit;
      return quantity.value === value && quantity.unit === nextUnit ? quantity : { dimension: "length", value, unit: nextUnit };
    }
    case "angle": {
      const nextUnit = unit === "deg" || unit === "rad" ? unit : quantity.unit;
      return quantity.value === value && quantity.unit === nextUnit ? quantity : { dimension: "angle", value, unit: nextUnit };
    }
    case "force": {
      const nextUnit = unit === "N" || unit === "kN" ? unit : quantity.unit;
      return quantity.value === value && quantity.unit === nextUnit ? quantity : { dimension: "force", value, unit: nextUnit };
    }
    case "pressure": {
      const nextUnit = unit === "MPa" || unit === "Pa" ? unit : quantity.unit;
      return quantity.value === value && quantity.unit === nextUnit ? quantity : { dimension: "pressure", value, unit: nextUnit };
    }
    case "mass": {
      const nextUnit = unit === "g" || unit === "kg" ? unit : quantity.unit;
      return quantity.value === value && quantity.unit === nextUnit ? quantity : { dimension: "mass", value, unit: nextUnit };
    }
  }
}

function formatParameterLiteral(parameter: ParameterDef): string {
  const value = parameter.value;
  if (value.type === "quantity") {
    const unit = "unit" in value.value ? value.value.unit : "";
    return `${formatNumber(value.value.value)}${unit}`;
  }
  if (value.type === "scalar" || value.type === "int") return formatNumber(value.value);
  if (value.type === "bool") return value.value ? "true" : "false";
  if (value.type === "string") return JSON.stringify(value.value);
  return parameter.name;
}

function formatNumber(value: number): string {
  if (!Number.isFinite(value)) return "0";
  const rounded = Math.round(value * 1e6) / 1e6;
  return String(rounded);
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}
