import { createContext, createElement, useCallback, useContext, useMemo, useRef, useState, type ReactNode } from "react";
import {
  applySessionCommands,
  canRedoSession,
  canUndoSession,
  createEngineeringSession,
  dispatchSessionCommand,
  redoSession,
  undoSession,
  type EngineeringSessionStore,
  type SessionOplogEntry,
  type WorkbenchSessionCommand,
} from "../core/session/engineeringSession.ts";

export {
  ATTACHED_CONTRACT_SUMMARY,
  canRedoSession,
  canUndoSession,
  createEngineeringSession,
  createMechanicalSessionGraph,
  dispatchSessionCommand,
  emptyVerificationReport,
  holeRadiusCommand,
  holeRadiusParameterName,
  MAIN_BRANCH,
  NO_DESIGN_CONTRACT_SUMMARY,
  parameterFromDimension,
  projectParameterIntoKcl,
  redoSession,
  STALE_VERIFICATION_SUMMARY,
  undoSession,
  applySessionCommands,
} from "../core/session/engineeringSession.ts";
export type {
  ChangePreview,
  ChangePreviewInput,
  EngineeringSessionState,
  EngineeringSessionStore,
  FeatureDiff,
  ParameterDiff,
  SemanticCommand,
  SessionOplogEntry,
  WorkbenchSessionCommand,
} from "../core/session/engineeringSession.ts";

export interface EngineeringSessionApi {
  readonly revision: number;
  readonly activeBranch: string;
  readonly activeProjectPath: string | null;
  readonly graphIr: NonNullable<EngineeringSessionStore["state"]["graphIr"]> | undefined;
  readonly kclSource: string;
  readonly verificationReport: EngineeringSessionStore["state"]["verificationReport"];
  readonly designContract: EngineeringSessionStore["state"]["designContract"];
  readonly selectedTopoRefs: readonly string[];
  readonly selectedNodeIds: readonly string[];
  readonly selectedNodeId: string | null;
  readonly preview: EngineeringSessionStore["state"]["preview"];
  readonly canUndo: boolean;
  readonly canRedo: boolean;
  setActiveProjectPath(path: string | null): void;
  dispatchSemanticCommand(command: WorkbenchSessionCommand, summary?: string): number | undefined;
  applySemanticCommands(commands: readonly WorkbenchSessionCommand[], summary?: string): number | undefined;
  undo(): SessionOplogEntry | undefined;
  redo(): SessionOplogEntry | undefined;
  getStore(): EngineeringSessionStore;
}

const EngineeringSessionContext = createContext<EngineeringSessionApi | null>(null);

export function EngineeringSessionProvider({ children }: { readonly children: ReactNode }) {
  const api = useEngineeringSessionState();
  return createElement(EngineeringSessionContext.Provider, { value: api }, children);
}

export function useEngineeringSession(): EngineeringSessionApi {
  const session = useContext(EngineeringSessionContext);
  if (!session) {
    throw new Error("useEngineeringSession must be used inside EngineeringSessionProvider");
  }
  return session;
}

function useEngineeringSessionState(): EngineeringSessionApi {
  const [store, setStore] = useState(createEngineeringSession);
  const [activeProjectPath, setActiveProjectPathState] = useState<string | null>(null);
  const storeRef = useRef(store);
  storeRef.current = store;

  const setActiveProjectPath = useCallback((path: string | null) => {
    const trimmed = typeof path === "string" ? path.trim() : "";
    setActiveProjectPathState(trimmed ? trimmed : null);
  }, []);

  const commit = useCallback((next: EngineeringSessionStore, previous: EngineeringSessionStore) => {
    storeRef.current = next;
    setStore(next);
    return next.state.revision !== previous.state.revision ? next.state.revision : undefined;
  }, []);

  const dispatchSemanticCommand = useCallback((command: WorkbenchSessionCommand, summary?: string) => {
    const previous = storeRef.current;
    return commit(dispatchSessionCommand(previous, command, summary), previous);
  }, [commit]);

  const applySemanticCommands = useCallback((commands: readonly WorkbenchSessionCommand[], summary?: string) => {
    const previous = storeRef.current;
    return commit(applySessionCommands(previous, commands, summary), previous);
  }, [commit]);

  const undo = useCallback(() => {
    const result = undoSession(storeRef.current);
    storeRef.current = result.store;
    setStore(result.store);
    return result.undone;
  }, []);

  const redo = useCallback(() => {
    const result = redoSession(storeRef.current);
    storeRef.current = result.store;
    setStore(result.store);
    return result.redone;
  }, []);

  return useMemo<EngineeringSessionApi>(() => ({
    revision: store.state.revision,
    activeBranch: store.state.activeBranch,
    activeProjectPath,
    graphIr: store.state.graphIr ?? undefined,
    kclSource: store.state.kclSource,
    verificationReport: store.state.verificationReport,
    designContract: store.state.designContract,
    selectedTopoRefs: store.state.selectedTopoRefs,
    selectedNodeIds: store.state.selectedNodeIds,
    selectedNodeId: store.state.selectedNodeId,
    preview: store.state.preview,
    canUndo: canUndoSession(store),
    canRedo: canRedoSession(store),
    setActiveProjectPath,
    dispatchSemanticCommand,
    applySemanticCommands,
    undo,
    redo,
    getStore: () => storeRef.current,
  }), [
    activeProjectPath,
    applySemanticCommands,
    dispatchSemanticCommand,
    redo,
    setActiveProjectPath,
    store,
    undo,
  ]);
}
