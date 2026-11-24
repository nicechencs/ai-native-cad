/**
 * Pure helpers for feature-tree patches.
 * Builders do not mutate the graph. `applyLocalGraphPatch` returns a new graph.
 */
import type {
  AngleUnit,
  CanonicalGraph,
  ForceUnit,
  GraphPatch,
  IrNode,
  IrType,
  IrValue,
  LengthUnit,
  MassUnit,
  NodeKind,
  NodeState,
  ParameterDef,
  PatchOp,
  PressureUnit,
  Quantity,
} from "../../types/graphIr.ts";

export interface RollbackHighlightSummary {
  readonly failedNodeIds: string[];
  readonly failedTopoRefs: string[];
  readonly suppressedNodeIds: string[];
}

const LENGTH_UNITS = new Set<LengthUnit>(["mm", "m", "in", "um", "cm"]);
const ANGLE_UNITS = new Set<AngleUnit>(["deg", "rad"]);
const FORCE_UNITS = new Set<ForceUnit>(["N", "kN"]);
const PRESSURE_UNITS = new Set<PressureUnit>(["MPa", "Pa"]);
const MASS_UNITS = new Set<MassUnit>(["g", "kg"]);

const DEFAULT_UNITS = {
  length: "mm",
  angle: "deg",
  force: "N",
  pressure: "MPa",
  mass: "kg",
} as const satisfies Record<Exclude<Quantity["dimension"], "density">, string>;

/**
 * Suppress a feature, or mark a suppressed feature stale so the evaluator recomputes it.
 * Any state other than `suppressed` becomes `suppressed`. `suppressed` becomes `stale`, not `valid`.
 */
export function buildToggleSuppressPatch(node: IrNode, patchId?: string): GraphPatch {
  const next: NodeState = node.state.status === "suppressed"
    ? { status: "stale" }
    : { status: "suppressed" };
  const verb = next.status === "suppressed" ? "Suppress" : "Unsuppress";
  return {
    patch_id: patchId ?? `toggle-suppress:${node.id}`,
    description: `${verb} ${node.id}`,
    ops: [
      {
        op: "set_node_state",
        value: { id: node.id, state: next },
      },
    ],
  };
}

/** Replace one parameter literal. The name, type, and formula stay as they are on `param`. */
export function buildUpdateParameterPatch(
  param: ParameterDef,
  newValue: unknown,
  patchId?: string,
): GraphPatch {
  const value = coerceParameterValue(param, newValue);
  return {
    patch_id: patchId ?? `update-parameter:${param.name}`,
    description: `Update parameter ${param.name}`,
    ops: [
      {
        op: "upsert_parameter",
        value: {
          name: param.name,
          ir_type: param.ir_type,
          value,
          // cad-ir requires `formula`. null means "literal only".
          formula: param.formula ?? null,
        },
      },
    ],
  };
}

/**
 * Collect failed and suppressed nodes for the viewport.
 * Failed topology refs are de-duplicated in node-id order so the same edge is highlighted once.
 * Node ids are sorted to match the canonical graph map order.
 */
export function buildRollbackHighlightSummary(graph: CanonicalGraph): RollbackHighlightSummary {
  const failedNodeIds: string[] = [];
  const failedTopoRefs: string[] = [];
  const suppressedNodeIds: string[] = [];
  const seenRefs = new Set<string>();

  const nodes = Object.values(graph.nodes).sort((left, right) => compareId(left.id, right.id));
  for (const node of nodes) {
    if (node.state.status === "failed_with_rollback") {
      failedNodeIds.push(node.id);
      for (const topoRef of node.state.failed_topo_refs) {
        if (seenRefs.has(topoRef)) continue;
        seenRefs.add(topoRef);
        failedTopoRefs.push(topoRef);
      }
      continue;
    }
    if (node.state.status === "suppressed") {
      suppressedNodeIds.push(node.id);
    }
  }

  return { failedNodeIds, failedTopoRefs, suppressedNodeIds };
}

/**
 * Apply a patch to a copy of `graph` for interactive tree updates.
 * `set_node_state` replaces one node state. `upsert_parameter` inserts or replaces by parameter name.
 * The input graph is left unchanged.
 */
export function applyLocalGraphPatch(graph: CanonicalGraph, patch: GraphPatch): CanonicalGraph {
  let next = graph;
  for (const op of patch.ops) {
    next = applyPatchOperation(next, op);
  }
  return next;
}

function applyPatchOperation(graph: CanonicalGraph, op: PatchOp): CanonicalGraph {
  switch (op.op) {
    case "upsert_parameter":
      return {
        ...graph,
        parameters: {
          ...graph.parameters,
          [op.value.name]: cloneValue(op.value),
        },
      };
    case "remove_parameter":
      return {
        ...graph,
        parameters: omitRecord(graph.parameters, op.value.name, `Cannot remove missing parameter "${op.value.name}"`),
      };
    case "upsert_blob":
      return {
        ...graph,
        blobs: {
          ...graph.blobs,
          [op.value.handle]: cloneValue(op.value),
        },
      };
    case "remove_blob":
      return {
        ...graph,
        blobs: omitRecord(graph.blobs, op.value.handle, `Cannot remove missing blob "${op.value.handle}"`),
      };
    case "upsert_external_ref":
      return {
        ...graph,
        external_refs: {
          ...(graph.external_refs ?? {}),
          [op.value.ref_id]: cloneValue(op.value),
        },
      };
    case "remove_external_ref":
      return {
        ...graph,
        external_refs: omitRecord(
          graph.external_refs ?? {},
          op.value.ref_id,
          `Cannot remove missing external reference "${op.value.ref_id}"`,
        ),
      };
    case "add_node":
      if (Object.hasOwn(graph.nodes, op.value.id)) {
        throw new TypeError(`Cannot add duplicate node "${op.value.id}"`);
      }
      return {
        ...graph,
        nodes: {
          ...graph.nodes,
          [op.value.id]: cloneValue(op.value),
        },
      };
    case "update_node":
      return {
        ...graph,
        nodes: {
          ...graph.nodes,
          [op.value.id]: updateNode(requireNode(graph, op.value.id, "update"), op.value),
        },
      };
    case "remove_node":
      return {
        ...graph,
        nodes: omitRecord(graph.nodes, op.value.id, `Cannot remove missing node "${op.value.id}"`),
      };
    case "set_node_state":
      return {
        ...graph,
        nodes: {
          ...graph.nodes,
          [op.value.id]: {
            ...requireNode(graph, op.value.id, "set state of"),
            state: cloneValue(op.value.state),
          },
        },
      };
    case "set_outputs":
      return { ...graph, outputs: [...op.value] };
    default: {
      const unreachable: never = op;
      return unreachable;
    }
  }
}

function requireNode(graph: CanonicalGraph, id: string, action: string): IrNode {
  const node = graph.nodes[id];
  if (!node) {
    throw new TypeError(`Cannot ${action} missing node "${id}"`);
  }
  return node;
}

function updateNode(
  node: IrNode,
  change: {
    readonly name: string | null;
    readonly inputs: Readonly<Record<string, IrValue>> | null;
    readonly kind: NodeKind | null;
  },
): IrNode {
  return {
    ...node,
    name: change.name ?? node.name,
    inputs: change.inputs === null ? node.inputs : cloneValue(change.inputs),
    kind: change.kind === null ? node.kind : cloneValue(change.kind),
  };
}

function omitRecord<T>(record: Readonly<Record<string, T>>, key: string, missingMessage: string): Record<string, T> {
  if (!Object.hasOwn(record, key)) {
    throw new TypeError(missingMessage);
  }
  return Object.fromEntries(Object.entries(record).filter(([name]) => name !== key));
}

function cloneValue<T>(value: T): T {
  return structuredClone(value);
}

function compareId(left: string, right: string): number {
  if (left < right) return -1;
  if (left > right) return 1;
  return 0;
}

function coerceParameterValue(param: ParameterDef, newValue: unknown): IrValue {
  if (looksLikeIrValue(newValue)) {
    const parsed = parseIrValue(newValue, param.name);
    if (!valueMatchesIrType(param.ir_type, parsed)) {
      throw new TypeError(
        `Cannot update parameter "${param.name}" (${param.ir_type}): IR value type ${parsed.type} does not match`,
      );
    }
    return parsed;
  }
  return coercePrimitive(param, newValue);
}

function coercePrimitive(param: ParameterDef, newValue: unknown): IrValue {
  switch (param.ir_type) {
    case "scalar":
      return { type: "scalar", value: expectFiniteNumber(param, newValue) };
    case "int":
      return { type: "int", value: expectInteger(param, newValue) };
    case "bool":
      if (typeof newValue !== "boolean") {
        throw mismatch(param, "expected a boolean or a typed IR value");
      }
      return { type: "bool", value: newValue };
    case "string":
      if (typeof newValue !== "string") {
        throw mismatch(param, "expected a string or a typed IR value");
      }
      return { type: "string", value: newValue };
    case "blob_ref":
      if (typeof newValue !== "string" || newValue.trim() === "") {
        throw mismatch(param, "expected a blob handle or a typed IR value");
      }
      return { type: "blob_ref", value: newValue };
    case "external_ref":
      if (typeof newValue !== "string" || newValue.trim() === "") {
        throw mismatch(param, "expected an external reference id or a typed IR value");
      }
      return { type: "external_ref", value: newValue };
    case "topo_ref":
      if (typeof newValue !== "string") {
        throw mismatch(param, "expected a topology expression or a typed IR value");
      }
      return { type: "topo_ref_expr", value: newValue };
    case "length":
    case "angle":
    case "force":
    case "pressure":
    case "mass":
    case "density":
      return coerceDimensioned(param, newValue);
    case "vec3_length":
    case "vec3_dir":
      return { type: "vec3", value: expectVec3(param, newValue) };
    default:
      throw mismatch(param, "expected a typed IR value");
  }
}

function coerceDimensioned(param: ParameterDef, newValue: unknown): IrValue {
  const numeric = expectFiniteNumber(param, newValue);
  switch (param.ir_type) {
    case "density":
      return { type: "quantity", value: { dimension: "density", value: numeric } };
    case "length":
      return { type: "quantity", value: { dimension: "length", value: numeric, unit: lengthUnit(param.value) ?? DEFAULT_UNITS.length } };
    case "angle":
      return { type: "quantity", value: { dimension: "angle", value: numeric, unit: angleUnit(param.value) ?? DEFAULT_UNITS.angle } };
    case "force":
      return { type: "quantity", value: { dimension: "force", value: numeric, unit: forceUnit(param.value) ?? DEFAULT_UNITS.force } };
    case "pressure":
      return { type: "quantity", value: { dimension: "pressure", value: numeric, unit: pressureUnit(param.value) ?? DEFAULT_UNITS.pressure } };
    case "mass":
      return { type: "quantity", value: { dimension: "mass", value: numeric, unit: massUnit(param.value) ?? DEFAULT_UNITS.mass } };
    default:
      throw mismatch(param, "expected a typed IR value");
  }
}

function lengthUnit(value: IrValue): LengthUnit | undefined {
  return value.type === "quantity" && value.value.dimension === "length" ? value.value.unit : undefined;
}

function angleUnit(value: IrValue): AngleUnit | undefined {
  return value.type === "quantity" && value.value.dimension === "angle" ? value.value.unit : undefined;
}

function forceUnit(value: IrValue): ForceUnit | undefined {
  return value.type === "quantity" && value.value.dimension === "force" ? value.value.unit : undefined;
}

function pressureUnit(value: IrValue): PressureUnit | undefined {
  return value.type === "quantity" && value.value.dimension === "pressure" ? value.value.unit : undefined;
}

function massUnit(value: IrValue): MassUnit | undefined {
  return value.type === "quantity" && value.value.dimension === "mass" ? value.value.unit : undefined;
}

function valueMatchesIrType(irType: IrType, value: IrValue): boolean {
  switch (irType) {
    case "scalar":
      return value.type === "scalar";
    case "int":
      return value.type === "int" && Number.isInteger(value.value);
    case "bool":
      return value.type === "bool";
    case "string":
      return value.type === "string";
    case "length":
    case "angle":
    case "force":
    case "pressure":
    case "mass":
    case "density":
      return value.type === "quantity" && value.value.dimension === irType;
    case "vec3_length":
    case "vec3_dir":
      return value.type === "vec3";
    case "vec2_length":
      return value.type === "list" && value.value.length === 2;
    case "blob_ref":
      return value.type === "blob_ref";
    case "external_ref":
      return value.type === "external_ref";
    case "topo_ref":
      return value.type === "topo_ref_expr";
    case "solid":
    case "surface":
    case "subd":
    case "scalar_field":
    case "vector_field":
    case "sketch":
    case "assembly":
    case "component":
      return value.type === "node_ref";
    default:
      return false;
  }
}

function looksLikeIrValue(value: unknown): value is Record<string, unknown> {
  return typeof value === "object"
    && value !== null
    && !Array.isArray(value)
    && "type" in value
    && typeof value.type === "string";
}

function parseIrValue(raw: Record<string, unknown>, paramName: string): IrValue {
  const tag = raw.type;
  const payload = raw.value;
  switch (tag) {
    case "scalar":
      return { type: "scalar", value: finiteFrom(payload, paramName) };
    case "int": {
      const parsed = finiteFrom(payload, paramName);
      if (!Number.isInteger(parsed)) {
        throw new TypeError(`Cannot update parameter "${paramName}" (int): expected an integer`);
      }
      return { type: "int", value: parsed };
    }
    case "bool":
      if (typeof payload !== "boolean") {
        throw new TypeError(`Cannot update parameter "${paramName}" (bool): expected a boolean`);
      }
      return { type: "bool", value: payload };
    case "string":
    case "blob_ref":
    case "external_ref":
    case "topo_ref_expr":
      if (typeof payload !== "string") {
        throw new TypeError(`Cannot update parameter "${paramName}": expected a string value`);
      }
      return { type: tag, value: payload };
    case "vec3":
      return { type: "vec3", value: readVec3(payload, paramName) };
    case "quantity":
      return { type: "quantity", value: readQuantity(payload, paramName) };
    case "node_ref":
      return { type: "node_ref", value: readNodeRef(payload, paramName) };
    case "list":
      if (!Array.isArray(payload)) {
        throw new TypeError(`Cannot update parameter "${paramName}": expected a list`);
      }
      return {
        type: "list",
        value: payload.map((item) => {
          if (!looksLikeIrValue(item)) {
            throw new TypeError(`Cannot update parameter "${paramName}": list items must be IR values`);
          }
          return parseIrValue(item, paramName);
        }),
      };
    case "map":
      if (!isPlainRecord(payload)) {
        throw new TypeError(`Cannot update parameter "${paramName}": expected a map`);
      }
      return {
        type: "map",
        value: Object.fromEntries(Object.entries(payload).map(([key, item]) => {
          if (!looksLikeIrValue(item)) {
            throw new TypeError(`Cannot update parameter "${paramName}": map values must be IR values`);
          }
          return [key, parseIrValue(item, paramName)];
        })),
      };
    default:
      throw new TypeError(`Cannot update parameter "${paramName}": unknown IR value type ${String(tag)}`);
  }
}

function readQuantity(payload: unknown, paramName: string): Quantity {
  if (!isPlainRecord(payload) || typeof payload.dimension !== "string") {
    throw new TypeError(`Cannot update parameter "${paramName}": quantity is missing a dimension`);
  }
  const numeric = finiteFrom(payload.value, paramName);
  switch (payload.dimension) {
    case "density":
      return { dimension: "density", value: numeric };
    case "length":
      return { dimension: "length", value: numeric, unit: readUnit(payload.unit, LENGTH_UNITS, paramName) };
    case "angle":
      return { dimension: "angle", value: numeric, unit: readUnit(payload.unit, ANGLE_UNITS, paramName) };
    case "force":
      return { dimension: "force", value: numeric, unit: readUnit(payload.unit, FORCE_UNITS, paramName) };
    case "pressure":
      return { dimension: "pressure", value: numeric, unit: readUnit(payload.unit, PRESSURE_UNITS, paramName) };
    case "mass":
      return { dimension: "mass", value: numeric, unit: readUnit(payload.unit, MASS_UNITS, paramName) };
    default:
      throw new TypeError(`Cannot update parameter "${paramName}": unknown quantity dimension ${payload.dimension}`);
  }
}

function readUnit<T extends string>(unit: unknown, allowed: ReadonlySet<T>, paramName: string): T {
  if (typeof unit === "string" && allowed.has(unit as T)) return unit as T;
  throw new TypeError(`Cannot update parameter "${paramName}": unit ${String(unit)} is not valid`);
}

function readNodeRef(payload: unknown, paramName: string): { readonly node_id: string; readonly port: "out" } {
  if (!isPlainRecord(payload) || typeof payload.node_id !== "string" || payload.node_id.trim() === "") {
    throw new TypeError(`Cannot update parameter "${paramName}": node reference needs a node id`);
  }
  if (payload.port !== "out") {
    throw new TypeError(`Cannot update parameter "${paramName}": node reference port must be "out"`);
  }
  return { node_id: payload.node_id, port: "out" };
}

function readVec3(payload: unknown, paramName: string): readonly [number, number, number] {
  if (!Array.isArray(payload) || payload.length !== 3) {
    throw new TypeError(`Cannot update parameter "${paramName}": expected 3 numbers`);
  }
  const [x, y, z] = payload;
  return [finiteFrom(x, paramName), finiteFrom(y, paramName), finiteFrom(z, paramName)];
}

function expectVec3(param: ParameterDef, newValue: unknown): readonly [number, number, number] {
  if (!Array.isArray(newValue)) {
    throw mismatch(param, "expected a typed IR value or 3 numbers");
  }
  return readVec3(newValue, param.name);
}

function expectFiniteNumber(param: ParameterDef, newValue: unknown): number {
  if (typeof newValue !== "number") {
    throw mismatch(param, "expected a finite number or a typed IR value");
  }
  return finiteFrom(newValue, param.name);
}

function expectInteger(param: ParameterDef, newValue: unknown): number {
  const numeric = expectFiniteNumber(param, newValue);
  if (!Number.isInteger(numeric)) {
    throw mismatch(param, "expected an integer");
  }
  return numeric;
}

function finiteFrom(value: unknown, paramName: string): number {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new TypeError(`Cannot update parameter "${paramName}": expected a finite number`);
  }
  return value;
}

function mismatch(param: ParameterDef, detail: string): TypeError {
  return new TypeError(`Cannot update parameter "${param.name}" (${param.ir_type}): ${detail}`);
}

function isPlainRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** The session graph stays mounted after KCL edits. An explicit graph still wins. */
export function graphIrForDocument<T>(
  source: string,
  demoSource: string,
  explicit: T | undefined,
  mounted: T | undefined,
): T | undefined {
  void source;
  void demoSource;
  return explicit ?? mounted;
}
