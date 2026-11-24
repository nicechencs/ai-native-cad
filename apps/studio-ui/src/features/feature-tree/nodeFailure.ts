/**
 * Failure details for a feature node.
 * A rollback keeps the last good solid and names the topology that should be marked in red.
 */
import type { IrNode } from "../../types/graphIr.ts";

export function readRecoveryHint(node: IrNode): string | null {
  if (node.state.status === "failed_with_rollback") {
    const fromState = node.state.recovery_hint?.trim();
    if (fromState) return fromState;
  }
  const fromMetadata = node.metadata.recovery_hint?.trim();
  return fromMetadata ? fromMetadata : null;
}

export function failedTopoRefsOf(node: IrNode): readonly string[] {
  return node.state.status === "failed_with_rollback" ? node.state.failed_topo_refs : [];
}
