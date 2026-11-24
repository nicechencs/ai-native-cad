/**
 * View model for the five-layer validation panel.
 * PASS is green, FAIL is red, and UNKNOWN is amber. A missing check stays UNKNOWN.
 */
import {
  VERIFICATION_LAYERS,
  type TriVerdict,
  type VerificationCheck,
  type VerificationLayer,
  type VerificationReport,
} from "../../types/verification.ts";

export type VerdictTone = "pass" | "fail" | "unknown";

export interface ValidationBadge {
  readonly layer: VerificationLayer;
  readonly shortLabel: "L0" | "L1" | "L2" | "L3" | "L4";
  readonly verdict: TriVerdict;
  readonly tone: VerdictTone;
}

export interface ValidationJumpTarget {
  readonly code: string;
  readonly message: string;
  readonly layer: VerificationLayer;
  readonly nodeId: string | null;
  readonly topoRef: string | null;
}

export interface ValidationPanelModel {
  readonly badges: readonly ValidationBadge[];
  readonly kernelLabel: string;
  readonly overall: TriVerdict;
  readonly overallTone: VerdictTone;
  readonly jumps: readonly ValidationJumpTarget[];
}

const SHORT_LABEL: Readonly<Record<VerificationLayer, ValidationBadge["shortLabel"]>> = {
  L0_structure_units: "L0",
  L1_geometry_topology: "L1",
  L2_interfaces_constraints: "L2",
  L3_manufacturability_physics: "L3",
  L4_interop_roundtrip: "L4",
};

export function verdictTone(verdict: TriVerdict): VerdictTone {
  switch (verdict) {
    case "PASS":
      return "pass";
    case "FAIL":
      return "fail";
    case "UNKNOWN":
      return "unknown";
  }
}

export function verdictClassName(verdict: TriVerdict): string {
  switch (verdict) {
    case "PASS":
      return "status-pill is-ok";
    case "FAIL":
      return "status-pill is-error";
    case "UNKNOWN":
      return "status-pill is-warn";
  }
}

export function kernelLabelOf(report: Pick<VerificationReport, "kernel_identity">): string {
  const provider = report.kernel_identity.provider.trim();
  const version = report.kernel_identity.version.trim();
  if (!provider || provider === "none") return "内核未连接";
  const name = provider.toLowerCase() === "occt" ? "OCCT" : provider;
  if (!version || version === "0") return name;
  return `${name} ${version}`;
}

export function buildValidationPanelModel(report: VerificationReport): ValidationPanelModel {
  const badges = VERIFICATION_LAYERS.map((layer) => {
    const verdict = report.layers[layer];
    return {
      layer,
      shortLabel: SHORT_LABEL[layer],
      verdict,
      tone: verdictTone(verdict),
    };
  });
  return {
    badges,
    kernelLabel: kernelLabelOf(report),
    overall: report.overall_status,
    overallTone: verdictTone(report.overall_status),
    jumps: report.checks.filter((check) => check.status === "FAIL").map(jumpTargetOf),
  };
}

function jumpTargetOf(check: VerificationCheck): ValidationJumpTarget {
  return {
    code: check.code,
    message: check.message,
    layer: check.layer,
    nodeId: readString(check.evidence, ["node_id", "nodeId"]),
    topoRef: readTopoRef(check.evidence),
  };
}

function readTopoRef(evidence: Readonly<Record<string, unknown>>): string | null {
  const direct = readString(evidence, ["topo_ref", "topoRef", "failed_topo_ref"]);
  if (direct) return direct;
  const list = evidence.failed_topo_refs ?? evidence.topo_refs ?? evidence.topoRefs;
  if (!Array.isArray(list)) return null;
  for (const item of list) {
    if (typeof item === "string" && item.trim()) return item.trim();
  }
  return null;
}

function readString(evidence: Readonly<Record<string, unknown>>, keys: readonly string[]): string | null {
  for (const key of keys) {
    const value = evidence[key];
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return null;
}
