/**
 * Display model and V2 payload parser for a candidate preview branch.
 * The card shows the plan, the parameter and feature diffs, the volume delta, and the five-layer gate.
 */
import type {
  ChangePreview,
  ChangePreviewInput,
  FeatureDiff,
  ParameterDiff,
  WorkbenchSessionCommand,
} from "../../core/session/engineeringSession.ts";
import { buildValidationPanelModel, type ValidationBadge } from "../validation/validationPanelModel.ts";
import {
  buildVerificationReport,
  isTriVerdict,
  isVerificationLayer,
  VERIFICATION_LAYERS,
  type KernelIdentity,
  type TriVerdict,
  type VerificationCheck,
  type VerificationLayer,
  type VerificationReport,
} from "../../types/verification.ts";

export interface ChangePreviewView {
  readonly previewBranch: string;
  readonly planSummary: string;
  readonly parameterLines: readonly string[];
  readonly featureLines: readonly string[];
  readonly volumeLabel: string;
  readonly overall: TriVerdict;
  readonly kernelLabel: string;
  readonly badges: readonly ValidationBadge[];
}

export interface V2ChangePreviewParseOptions {
  readonly mode: "generate" | "edit";
  readonly candidateSource?: string;
  readonly fallbackPrompt?: string;
  readonly fallbackProjectPath?: string | null;
}

export function buildChangePreviewView(preview: ChangePreview): ChangePreviewView {
  const validation = buildValidationPanelModel(preview.verificationReport);
  return {
    previewBranch: preview.previewBranch,
    planSummary: preview.planSummary,
    parameterLines: preview.parameterDiffs.map(formatParameterDiff),
    featureLines: preview.featureDiffs.map(formatFeatureDiff),
    volumeLabel: formatVolumeDelta(preview.volumeDeltaMm3),
    overall: validation.overall,
    kernelLabel: validation.kernelLabel,
    badges: validation.badges,
  };
}

/**
 * Returns true only for a hard geometric/verification FAIL.
 * Honest UNKNOWN (such as L4 on non-OCCT builds) is NOT a hard failure and may be previewed.
 */
export function isHardFailedV2Preview(raw: unknown): boolean {
  if (!isRecord(raw)) return false;
  const verdict = readVerdict(raw.verdict ?? raw.overall_status);
  if (verdict === "FAIL") return true;
  if (isRecord(raw.verification)) {
    const verificationVerdict = readVerdict(raw.verification.overall_status);
    if (verificationVerdict === "FAIL") return true;
  }
  return false;
}

/**
 * Parse a V2 `changePreview` payload from Web `dev-server.mjs` or Desktop Tauri native `run_ai_agent`
 * into the unified `ChangePreviewInput` consumed by `EngineeringSession` and `ChangePreviewCard`.
 */
export function parseV2ChangePreview(
  raw: unknown,
  options: V2ChangePreviewParseOptions,
): ChangePreviewInput | null {
  if (!isRecord(raw)) return null;
  const previewBranch =
    readNonEmptyString(raw.preview_branch) ??
    readNonEmptyString(raw.branch) ??
    `ai/preview-${options.mode}`;
  const baseBranch = readNonEmptyString(raw.base_branch) ?? "main";
  const projectPath = readNonEmptyString(raw.project_path) ?? options.fallbackProjectPath ?? null;
  const candidateSource = options.candidateSource?.trim() ? options.candidateSource : undefined;

  const verificationReport = parseVerificationReport(raw.verification, raw.verdict, raw.passed);
  const { parameterDiffs, featureDiffs, volumeDeltaMm3 } = parseDiffRecord(raw.diff);

  const explanation = readNonEmptyString(raw.explanation);
  const promptSummary = readNonEmptyString(options.fallbackPrompt);
  const planSummary =
    explanation ??
    (promptSummary
      ? `相对 ${baseBranch} 预览：${promptSummary}`
      : `相对 ${baseBranch} 审阅候选分支 ${previewBranch}`);

  const commands: WorkbenchSessionCommand[] = [];
  if (candidateSource) {
    commands.push(
      options.mode === "generate"
        ? { op: "load_kcl_document", source: candidateSource }
        : { op: "sync_kcl_view", source: candidateSource },
    );
  }

  return {
    previewBranch,
    planSummary,
    parameterDiffs,
    featureDiffs,
    volumeDeltaMm3,
    verificationReport,
    commands,
    projectPath,
    ...(candidateSource ? { candidateSource } : {}),
    mode: options.mode,
  };
}

function parseVerificationReport(
  rawVerification: unknown,
  rawVerdict: unknown,
  rawPassed: unknown,
): VerificationReport {
  if (isRecord(rawVerification)) {
    const rawKernel = isRecord(rawVerification.kernel_identity)
      ? rawVerification.kernel_identity
      : isRecord(rawVerification.kernel)
        ? rawVerification.kernel
        : null;
    const kernel_identity: KernelIdentity = {
      provider: readNonEmptyString(rawKernel?.provider) ?? "unknown",
      version: readNonEmptyString(rawKernel?.version) ?? "0",
      authoritative: rawKernel?.authoritative === true,
    };

    const checks: VerificationCheck[] = [];
    if (Array.isArray(rawVerification.checks)) {
      for (const item of rawVerification.checks) {
        if (!isRecord(item)) continue;
        const layer = item.layer;
        const status = readVerdict(item.status);
        const code = readNonEmptyString(item.code);
        const message = readNonEmptyString(item.message);
        if (!isVerificationLayer(layer) || !status || !code || !message) continue;
        checks.push({
          code,
          layer,
          status,
          message,
          evidence: isRecord(item.evidence) ? item.evidence : {},
        });
      }
    }

    const layerOverrides: Partial<Record<VerificationLayer, TriVerdict>> = {};
    if (isRecord(rawVerification.layers)) {
      for (const layer of VERIFICATION_LAYERS) {
        const verdict = readVerdict(rawVerification.layers[layer]);
        if (verdict) layerOverrides[layer] = verdict;
      }
    }

    const summary = readNonEmptyString(rawVerification.summary) ?? undefined;
    return buildVerificationReport({
      checks,
      kernel_identity,
      layers: layerOverrides,
      ...(summary ? { summary } : {}),
    });
  }

  const verdict = readVerdict(rawVerdict) ?? (rawPassed === true ? "PASS" : "UNKNOWN");
  if (verdict === "PASS") {
    return buildVerificationReport({
      checks: [],
      kernel_identity: { provider: "cad-runtime", version: "v2", authoritative: false },
      layers: {
        L0_structure_units: "PASS",
        L1_geometry_topology: "PASS",
        L2_interfaces_constraints: "PASS",
        L3_manufacturability_physics: "PASS",
        L4_interop_roundtrip: "PASS",
      },
      summary: "V2 预览校验通过",
    });
  }
  if (verdict === "FAIL") {
    return buildVerificationReport({
      checks: [
        {
          code: "V2_PREVIEW_FAIL",
          layer: "L1_geometry_topology",
          status: "FAIL",
          message: "V2 候选分支未通过几何门禁校验",
          evidence: {},
        },
      ],
      kernel_identity: { provider: "cad-runtime", version: "v2", authoritative: false },
      summary: "V2 预览门禁未通过",
    });
  }
  return buildVerificationReport({
    checks: [],
    kernel_identity: { provider: "cad-runtime", version: "v2", authoritative: false },
    summary: "V2 预览已完成（部分门禁为 UNKNOWN，请审阅后确认合入）",
  });
}

function parseDiffRecord(rawDiff: unknown): {
  readonly parameterDiffs: ParameterDiff[];
  readonly featureDiffs: FeatureDiff[];
  readonly volumeDeltaMm3: number;
} {
  if (!isRecord(rawDiff)) {
    return { parameterDiffs: [], featureDiffs: [], volumeDeltaMm3: 0 };
  }

  const parameterDiffs: ParameterDiff[] = [];
  if (Array.isArray(rawDiff.parameter_diffs)) {
    for (const entry of rawDiff.parameter_diffs) {
      if (!isRecord(entry)) continue;
      const name = readNonEmptyString(entry.name);
      if (!name) continue;
      parameterDiffs.push({
        name,
        before: String(entry.before ?? "（无）"),
        after: String(entry.after ?? "（无）"),
      });
    }
  } else {
    for (const name of readStrings(rawDiff.added_parameters)) {
      parameterDiffs.push({ name, before: "（无）", after: "新增" });
    }
    for (const name of readStrings(rawDiff.removed_parameters)) {
      parameterDiffs.push({ name, before: "存在", after: "删除" });
    }
    for (const name of readStrings(rawDiff.changed_parameters)) {
      parameterDiffs.push({ name, before: "旧值", after: "新值" });
    }
  }

  const featureDiffs: FeatureDiff[] = [];
  if (Array.isArray(rawDiff.feature_diffs)) {
    for (const entry of rawDiff.feature_diffs) {
      if (!isRecord(entry)) continue;
      const nodeId = readNonEmptyString(entry.nodeId ?? entry.node_id);
      const change = entry.change;
      if (
        !nodeId ||
        (change !== "added" &&
          change !== "removed" &&
          change !== "modified" &&
          change !== "renamed" &&
          change !== "suppressed" &&
          change !== "unsuppressed")
      ) {
        continue;
      }
      featureDiffs.push({
        nodeId,
        change,
        summary: readNonEmptyString(entry.summary) ?? "预览分支变更",
      });
    }
  } else {
    for (const nodeId of readStrings(rawDiff.added_nodes)) {
      featureDiffs.push({ nodeId, change: "added", summary: "预览分支新增" });
    }
    for (const nodeId of readStrings(rawDiff.removed_nodes)) {
      featureDiffs.push({ nodeId, change: "removed", summary: "预览分支删除" });
    }
    for (const nodeId of readStrings(rawDiff.changed_nodes)) {
      featureDiffs.push({ nodeId, change: "modified", summary: "预览分支修改" });
    }
  }

  let volumeDeltaMm3 = 0;
  if (typeof rawDiff.volume_delta_mm3 === "number" && Number.isFinite(rawDiff.volume_delta_mm3)) {
    volumeDeltaMm3 = rawDiff.volume_delta_mm3;
  } else if (isRecord(rawDiff.volume_mm3)) {
    const before = rawDiff.volume_mm3.before;
    const after = rawDiff.volume_mm3.after;
    if (typeof before === "number" && Number.isFinite(before) && typeof after === "number" && Number.isFinite(after)) {
      volumeDeltaMm3 = after - before;
    }
  }

  return { parameterDiffs, featureDiffs, volumeDeltaMm3 };
}

function readVerdict(value: unknown): TriVerdict | null {
  if (typeof value !== "string") return null;
  const upper = value.trim().toUpperCase();
  return isTriVerdict(upper) ? upper : null;
}

function readNonEmptyString(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const trimmed = value.trim();
  return trimmed ? trimmed : null;
}

function readStrings(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value
    .filter((entry): entry is string => typeof entry === "string" && entry.trim().length > 0)
    .map((entry) => entry.trim());
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function formatParameterDiff(diff: ParameterDiff): string {
  return `${diff.name}: ${diff.before} → ${diff.after}`;
}

function formatFeatureDiff(diff: FeatureDiff): string {
  return `${diff.nodeId} · ${diff.change} · ${diff.summary}`;
}

export function formatVolumeDelta(value: number): string {
  if (!Number.isFinite(value)) return "体积变化未知";
  const sign = value > 0 ? "+" : "";
  const rounded = Math.round(value * 1000) / 1000;
  return `${sign}${rounded} mm³`;
}
