/**
 * Pure mappers from Studio UI workbench actions (`WorkbenchSessionCommand`,
 * `GraphPatch`, `ParameterDef`, `DesignContract`, `ChangePreview`) to strongly
 * typed `CadApiSemanticCommand[]` payloads for Live-Attach and `cad-cli api apply`.
 *
 * UI/view-only commands (`select`, `sync_kcl_view`, `open_preview`, etc.) never
 * emit backend CAD commands and are either skipped or rejected in `strict` mode.
 */
import type {
  ApiDesignContract,
  CadApiSemanticCommand,
} from "../../types/apiCommands.ts";
import type { GraphPatch, ParameterDef, PatchOp } from "../../types/graphIr.ts";
import type { DesignContract } from "../../types/scene.ts";
import type { ChangePreview, WorkbenchSessionCommand } from "./engineeringSession.ts";

export class CadApiAdapterError extends Error {
  readonly code: string;
  readonly op: string;

  constructor(code: string, op: string, message: string) {
    super(message);
    this.name = "CadApiAdapterError";
    this.code = code;
    this.op = op;
  }
}

export interface CadApiAdapterOptions {
  /**
   * When true, throw `CadApiAdapterError` if any command or `PatchOp` has no
   * lossless `cad-api::SemanticCommand` equivalent. Defaults to false.
   */
  readonly strict?: boolean;
}

export interface SkippedWorkbenchCommand {
  readonly op: string;
  readonly reason: string;
}

export interface CadApiTranslationResult {
  readonly commands: readonly CadApiSemanticCommand[];
  readonly skipped: readonly SkippedWorkbenchCommand[];
}

/** Map a Studio `DesignContract` (`scene.ts`) onto `cad_api::DesignContract`. */
export function toApiDesignContract(contract: DesignContract): ApiDesignContract {
  const partName = contract.part_name.trim();
  if (!partName) {
    throw new CadApiAdapterError(
      "INVALID_DESIGN_CONTRACT",
      "set_design_contract",
      "DesignContract.part_name cannot be empty.",
    );
  }
  const material = typeof contract.material === "string"
    ? contract.material
    : contract.material.name;

  return {
    part_name: partName,
    ...(contract.summary !== undefined ? { summary: contract.summary } : {}),
    ...(material ? { material } : {}),
    ...(contract.manufacturing_method
      ? { manufacturing_method: contract.manufacturing_method }
      : {}),
  };
}

/** Map a `ParameterDef` onto `cad_api::SemanticCommand::UpsertParameter`. */
export function parameterDefToCadApiCommand(parameter: ParameterDef): CadApiSemanticCommand {
  const name = parameter.name.trim();
  if (!name) {
    throw new CadApiAdapterError(
      "INVALID_PARAMETER_NAME",
      "upsert_parameter",
      "Parameter name cannot be empty.",
    );
  }
  return {
    command: "upsert_parameter",
    args: {
      name,
      ir_type: parameter.ir_type,
      value: parameter.value,
      ...(parameter.formula != null ? { formula: parameter.formula } : {}),
    },
  };
}

/**
 * Map a single `GraphPatch` `PatchOp` onto a `CadApiSemanticCommand` when a
 * 1:1 semantic equivalent exists, or `null` for low-level graph structural ops.
 */
export function patchOpToCadApiCommand(op: PatchOp): CadApiSemanticCommand | null {
  switch (op.op) {
    case "upsert_parameter":
      return parameterDefToCadApiCommand(op.value);
    case "remove_parameter": {
      const name = op.value.name.trim();
      if (!name) {
        throw new CadApiAdapterError(
          "INVALID_PARAMETER_NAME",
          "remove_parameter",
          "Parameter name cannot be empty.",
        );
      }
      return {
        command: "remove_parameter",
        args: { name },
      };
    }
    case "remove_node":
      return {
        command: "delete_node",
        args: { node_id: op.value.id },
      };
    case "set_node_state":
      if (op.value.state.status === "suppressed") {
        return {
          command: "set_node_suppressed",
          args: { node_id: op.value.id, suppressed: true },
        };
      }
      if (op.value.state.status === "stale") {
        return {
          command: "set_node_suppressed",
          args: { node_id: op.value.id, suppressed: false },
        };
      }
      return null;
    case "update_node":
      if (op.value.name !== null && op.value.inputs === null && op.value.kind === null) {
        const name = op.value.name.trim();
        if (!name) {
          throw new CadApiAdapterError(
            "INVALID_NODE_NAME",
            "update_node",
            "Node name cannot be empty.",
          );
        }
        return {
          command: "rename_node",
          args: { node_id: op.value.id, name },
        };
      }
      return null;
    case "add_node":
    case "upsert_blob":
    case "remove_blob":
    case "upsert_external_ref":
    case "remove_external_ref":
    case "set_outputs":
      return null;
    default: {
      const unreachable: never = op;
      return unreachable;
    }
  }
}

/** Translate a `GraphPatch` into `CadApiSemanticCommand[]`. */
export function graphPatchToCadApiCommands(
  patch: GraphPatch,
  options?: CadApiAdapterOptions,
): readonly CadApiSemanticCommand[] {
  return translateGraphPatch(patch, options).commands;
}

/** Translate one `WorkbenchSessionCommand` into `CadApiSemanticCommand[]` plus skipped metadata. */
export function translateWorkbenchCommand(
  command: WorkbenchSessionCommand,
  options?: CadApiAdapterOptions,
): CadApiTranslationResult {
  const strict = options?.strict === true;

  switch (command.op) {
    case "upsert_parameter":
      return {
        commands: [parameterDefToCadApiCommand(command.value)],
        skipped: [],
      };
    case "remove_parameter": {
      const name = command.name.trim();
      if (!name) {
        throw new CadApiAdapterError(
          "INVALID_PARAMETER_NAME",
          command.op,
          "Parameter name cannot be empty.",
        );
      }
      return {
        commands: [{ command: "remove_parameter", args: { name } }],
        skipped: [],
      };
    }
    case "rename_node": {
      const name = command.name.trim();
      if (!name) {
        throw new CadApiAdapterError(
          "INVALID_NODE_NAME",
          command.op,
          "Node name cannot be empty.",
        );
      }
      return {
        commands: [{ command: "rename_node", args: { node_id: command.nodeId, name } }],
        skipped: [],
      };
    }
    case "set_node_suppressed":
      return {
        commands: [
          {
            command: "set_node_suppressed",
            args: { node_id: command.nodeId, suppressed: command.suppressed },
          },
        ],
        skipped: [],
      };
    case "set_design_contract":
      if (command.contract === null) {
        return skipOrThrow(
          strict,
          command.op,
          "UNSUPPORTED_NULL_CONTRACT",
          "Clearing design contract has no direct cad-api::SemanticCommand equivalent.",
        );
      }
      return {
        commands: [
          {
            command: "set_design_contract",
            args: { contract: toApiDesignContract(command.contract) },
          },
        ],
        skipped: [],
      };
    case "apply_graph_patch":
      return translateGraphPatch(command.patch, options);
    case "select":
    case "set_verification_report":
    case "sync_kcl_view":
    case "load_kcl_document":
    case "set_active_branch":
    case "open_preview":
    case "commit_preview":
    case "discard_preview":
    case "reset_document":
      return skipOrThrow(
        strict,
        command.op,
        "UI_ONLY_COMMAND",
        `Workbench command "${command.op}" is a UI/session-only operation and has no cad-api write command.`,
      );
    default: {
      const unreachable: never = command;
      return unreachable;
    }
  }
}

/** Translate a batch of `WorkbenchSessionCommand` values into `CadApiSemanticCommand[]`. */
export function translateWorkbenchCommands(
  commands: readonly WorkbenchSessionCommand[],
  options?: CadApiAdapterOptions,
): CadApiTranslationResult {
  const translated: CadApiSemanticCommand[] = [];
  const skipped: SkippedWorkbenchCommand[] = [];
  for (const command of commands) {
    const step = translateWorkbenchCommand(command, options);
    translated.push(...step.commands);
    skipped.push(...step.skipped);
  }
  return { commands: translated, skipped };
}

/** Convenience mapper returning only the resulting `CadApiSemanticCommand[]`. */
export function workbenchCommandsToCadApi(
  commands: readonly WorkbenchSessionCommand[],
  options?: CadApiAdapterOptions,
): readonly CadApiSemanticCommand[] {
  return translateWorkbenchCommands(commands, options).commands;
}

/** Convert a `ChangePreview`'s queued commands into `CadApiSemanticCommand[]`. */
export function previewToCadApiCommands(
  preview: Pick<ChangePreview, "commands">,
  options?: CadApiAdapterOptions,
): readonly CadApiSemanticCommand[] {
  return workbenchCommandsToCadApi(preview.commands, options);
}

/**
 * Serialize a non-empty `CadApiSemanticCommand` batch for Live-Attach (`commands_json`)
 * or `cad-cli api apply --commands <json>`.
 */
export function formatCadApiApplyPayload(commands: readonly CadApiSemanticCommand[]): string {
  if (commands.length === 0) {
    throw new CadApiAdapterError(
      "EMPTY_COMMAND_BATCH",
      "formatCadApiApplyPayload",
      "cad-api batch must contain at least one SemanticCommand.",
    );
  }
  return JSON.stringify(commands);
}

function translateGraphPatch(
  patch: GraphPatch,
  options?: CadApiAdapterOptions,
): CadApiTranslationResult {
  const strict = options?.strict === true;
  const commands: CadApiSemanticCommand[] = [];
  const skipped: SkippedWorkbenchCommand[] = [];

  for (const op of patch.ops) {
    const mapped = patchOpToCadApiCommand(op);
    if (mapped) {
      commands.push(mapped);
    } else {
      const step = skipOrThrow(
        strict,
        `apply_graph_patch:${op.op}`,
        "UNSUPPORTED_PATCH_OP",
        `PatchOp "${op.op}" in patch "${patch.patch_id}" cannot be mapped losslessly to cad-api::SemanticCommand.`,
      );
      skipped.push(...step.skipped);
    }
  }

  return { commands, skipped };
}

function skipOrThrow(
  strict: boolean,
  op: string,
  code: string,
  reason: string,
): CadApiTranslationResult {
  if (strict) {
    throw new CadApiAdapterError(code, op, reason);
  }
  return {
    commands: [],
    skipped: [{ op, reason }],
  };
}
