/**
 * TES-1-P0.5-UI-CONTAINER-UNDO
 *
 * After opening a SQLite `.aicad`, in-memory session Undo only undoes "打开".
 * Modeling Undo of a save made in this session must roll the container back.
 * Unsaved editor edits must be undone first; pre-open oplog must not be
 * rolled back by the first Ctrl+Z after Open.
 */
export interface ContainerUndoHistory {
  readonly canRollback: boolean;
  readonly oplogLen: number;
  readonly headSeq: number | null;
  readonly previousSeq: number | null;
}

export interface ContainerUndoOptions {
  readonly hasUnsavedEdits: boolean;
  readonly baselineHeadSeq: number | null;
}

export function shouldUndoContainerHistory(
  history: ContainerUndoHistory | null | undefined,
  options: ContainerUndoOptions,
): boolean {
  if (history?.canRollback !== true) return false;
  if (options.hasUnsavedEdits) return false;
  if (options.baselineHeadSeq === null || history.headSeq === null) return false;
  return history.headSeq !== options.baselineHeadSeq;
}

export function historyFromIpc(raw?: {
  readonly can_rollback?: boolean;
  readonly oplog_len?: number;
  readonly head_seq?: number | null;
  readonly previous_seq?: number | null;
} | null): ContainerUndoHistory {
  return {
    canRollback: raw?.can_rollback === true,
    oplogLen: raw?.oplog_len ?? 0,
    headSeq: raw?.head_seq ?? null,
    previousSeq: raw?.previous_seq ?? null,
  };
}
