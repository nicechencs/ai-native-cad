/**
 * TES-1 / S12 — when may the workbench apply a quick feature to the current
 * KCL source using the current viewport selection?
 *
 * The displayed mesh may be the last accepted solid after a compile error.
 * Selection ids from that mesh must not be written into a different / failing
 * source (agy 0838 / Path D).
 */
export interface QuickFeatureApplyInput {
  readonly diagnosticsStatus?: string;
  readonly showingLastAccepted: boolean;
  readonly transportError: boolean;
  readonly compilePending?: boolean;
  readonly displayedRevision?: number;
  readonly sourceRevision?: number;
}

export type QuickFeatureApplyDecision =
  | { readonly ok: true }
  | { readonly ok: false; readonly reason: string };

const STALE_SELECTION_REASON =
  "当前源码未编译通过，不能用上一份几何的选择改写当前源码。";
const PENDING_REASON =
  "当前源码尚未完成编译，不能用上一份几何的选择改写当前源码。";
const TRANSPORT_REASON =
  "工作台传输失败，不能把上一份几何的选择写进当前源码。";

export function canApplyQuickFeature(input: QuickFeatureApplyInput): QuickFeatureApplyDecision {
  if (input.transportError) {
    return { ok: false, reason: TRANSPORT_REASON };
  }
  if (input.showingLastAccepted || input.diagnosticsStatus === "error") {
    return { ok: false, reason: STALE_SELECTION_REASON };
  }
  if (input.diagnosticsStatus !== "ok") {
    return { ok: false, reason: STALE_SELECTION_REASON };
  }
  const revisionsKnown =
    input.displayedRevision !== undefined && input.sourceRevision !== undefined;
  if (input.compilePending === true || (revisionsKnown && input.displayedRevision !== input.sourceRevision)) {
    return { ok: false, reason: PENDING_REASON };
  }
  return { ok: true };
}

export type QuickFeatureSelection =
  | { readonly kind: "face"; readonly face: { readonly face_id?: string } }
  | { readonly kind: "edge"; readonly edge: { readonly edge_id?: string } }
  | null
  | undefined;

/** Only a real current selection may be used. Never invent faces[0] or the first solid. */
export function selectedQuickFeatureTargetId(selectedTarget: QuickFeatureSelection): string | null {
  if (selectedTarget?.kind === "face") {
    const id = selectedTarget.face.face_id?.trim();
    return id ? id : null;
  }
  if (selectedTarget?.kind === "edge") {
    const id = selectedTarget.edge.edge_id?.trim();
    return id ? id : null;
  }
  return null;
}
