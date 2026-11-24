/**
 * Five-layer tri-state (PASS / FAIL / UNKNOWN) engineering verification contracts.
 * Strictly aligned with `schemas/verification-report.schema.json` and `crates/cad-ir`.
 */

export const VERIFICATION_REPORT_SCHEMA_VERSION = 1;

export type TriVerdict = "PASS" | "FAIL" | "UNKNOWN";

export const TRI_VERDICTS: readonly TriVerdict[] = [
  "PASS",
  "FAIL",
  "UNKNOWN",
] as const;

export type VerificationLayer =
  | "L0_structure_units"
  | "L1_geometry_topology"
  | "L2_interfaces_constraints"
  | "L3_manufacturability_physics"
  | "L4_interop_roundtrip";

export const VERIFICATION_LAYERS: readonly VerificationLayer[] = [
  "L0_structure_units",
  "L1_geometry_topology",
  "L2_interfaces_constraints",
  "L3_manufacturability_physics",
  "L4_interop_roundtrip",
] as const;

export interface KernelIdentity {
  readonly provider: string;
  readonly version: string;
  readonly authoritative: boolean;
}

export interface VerificationCheck {
  readonly code: string;
  readonly layer: VerificationLayer;
  readonly status: TriVerdict;
  readonly message: string;
  readonly evidence: Readonly<Record<string, unknown>>;
}

export type VerificationLayerMap = Readonly<Record<VerificationLayer, TriVerdict>>;

export interface VerificationReport {
  readonly schema_version: number;
  readonly overall_status: TriVerdict;
  readonly kernel_identity: KernelIdentity;
  readonly layers: VerificationLayerMap;
  readonly checks: readonly VerificationCheck[];
  readonly summary?: string | null;
}

export function isTriVerdict(value: unknown): value is TriVerdict {
  return value === "PASS" || value === "FAIL" || value === "UNKNOWN";
}

export function isVerificationLayer(value: unknown): value is VerificationLayer {
  return (
    typeof value === "string" &&
    (VERIFICATION_LAYERS as readonly string[]).includes(value)
  );
}

/**
 * Pure zero-trust tri-state aggregation:
 * - Any `FAIL` yields `FAIL`
 * - Otherwise any `UNKNOWN` (or empty input) yields `UNKNOWN`
 * - Only when every item is `PASS` does it yield `PASS`
 */
export function aggregateTriVerdicts(verdicts: Iterable<TriVerdict>): TriVerdict {
  let count = 0;
  let hasUnknown = false;
  for (const verdict of verdicts) {
    count += 1;
    if (verdict === "FAIL") {
      return "FAIL";
    }
    if (verdict === "UNKNOWN") {
      hasUnknown = true;
    }
  }
  if (count === 0 || hasUnknown) {
    return "UNKNOWN";
  }
  return "PASS";
}

/**
 * Compute per-layer tri-state verdicts from individual checks and optional explicit layer overrides.
 * Any layer without checks and without an override defaults to `UNKNOWN` (never silently `PASS`).
 */
export function aggregateLayerStatuses(
  checks: readonly VerificationCheck[],
  overrides?: Partial<Record<VerificationLayer, TriVerdict>>,
): Record<VerificationLayer, TriVerdict> {
  const grouped: Record<VerificationLayer, TriVerdict[]> = {
    L0_structure_units: [],
    L1_geometry_topology: [],
    L2_interfaces_constraints: [],
    L3_manufacturability_physics: [],
    L4_interop_roundtrip: [],
  };

  for (const check of checks) {
    if (isVerificationLayer(check.layer)) {
      grouped[check.layer].push(check.status);
    }
  }

  const layers = {} as Record<VerificationLayer, TriVerdict>;
  for (const layer of VERIFICATION_LAYERS) {
    const layerChecks = grouped[layer];
    const override = overrides?.[layer];
    if (layerChecks.length > 0 && override !== undefined) {
      layers[layer] = aggregateTriVerdicts([...layerChecks, override]);
    } else if (override !== undefined) {
      layers[layer] = override;
    } else {
      layers[layer] = aggregateTriVerdicts(layerChecks);
    }
  }
  return layers;
}

/**
 * Aggregate the five canonical layer verdicts into the report's `overall_status`.
 */
export function computeOverallVerdict(layers: VerificationLayerMap): TriVerdict {
  return aggregateTriVerdicts(VERIFICATION_LAYERS.map((layer) => layers[layer]));
}

/**
 * Construct a schema-compliant `VerificationReport` with deterministic layer and overall status aggregation.
 */
export function buildVerificationReport(params: {
  readonly checks: readonly VerificationCheck[];
  readonly kernel_identity: KernelIdentity;
  readonly layers?: Partial<Record<VerificationLayer, TriVerdict>>;
  readonly summary?: string | null;
  readonly schema_version?: number;
}): VerificationReport {
  const layers = aggregateLayerStatuses(params.checks, params.layers);
  const overall_status = computeOverallVerdict(layers);
  const report: VerificationReport = {
    schema_version: params.schema_version ?? VERIFICATION_REPORT_SCHEMA_VERSION,
    overall_status,
    kernel_identity: {
      provider: params.kernel_identity.provider,
      version: params.kernel_identity.version,
      authoritative: params.kernel_identity.authoritative,
    },
    layers,
    checks: [...params.checks],
    ...(params.summary !== undefined ? { summary: params.summary } : {}),
  };
  return report;
}

/**
 * Returns true if and only if `overall_status === "PASS"` and all five layers are `"PASS"`.
 */
export function isVerificationReportPassed(
  report: Pick<VerificationReport, "overall_status" | "layers">,
): boolean {
  return (
    report.overall_status === "PASS" &&
    computeOverallVerdict(report.layers) === "PASS"
  );
}
