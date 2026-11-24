import type { WorkbenchModel } from "../sync/workbenchBackend.ts";

export interface SolidSelection {
  readonly live: WorkbenchModel | undefined;
  readonly retained: WorkbenchModel | undefined;
  readonly model: WorkbenchModel | undefined;
  readonly pending: boolean;
  /** The viewport, metrics, and export note are the last accepted solid. */
  readonly showingLastAccepted: boolean;
}

/**
 * The compiler rolls a failed feature back to the geometry still in the
 * source. Replacing an accepted fillet with a rejected radius therefore
 * returns the solid from before that fillet, not the solid the kernel last
 * accepted. The workbench keeps the last accepted solid on screen instead.
 */
export function selectDisplayedSolid(
  live: WorkbenchModel | undefined,
  retained: WorkbenchModel | undefined,
): SolidSelection {
  const accepted = live && isAcceptedSolid(live) ? live : retained;

  if (live === undefined) {
    return {
      live,
      retained: accepted,
      model: accepted,
      pending: accepted !== undefined,
      showingLastAccepted: accepted !== undefined,
    };
  }

  if (live.transportError || live.diagnostics.status === "ok") {
    return {
      live,
      retained: accepted,
      model: live,
      pending: false,
      showingLastAccepted: false,
    };
  }

  if (accepted && accepted.sceneMesh.faces.length > 0) {
    return {
      live,
      retained: accepted,
      model: withAcceptedGeometry(live, accepted),
      pending: false,
      showingLastAccepted: true,
    };
  }

  return {
    live,
    retained: accepted,
    model: live,
    pending: false,
    showingLastAccepted: false,
  };
}

function isAcceptedSolid(model: WorkbenchModel): boolean {
  return model.diagnostics.status === "ok"
    && !model.transportError
    && model.sceneMesh.faces.length > 0;
}

function withAcceptedGeometry(failed: WorkbenchModel, accepted: WorkbenchModel): WorkbenchModel {
  return {
    origin: accepted.origin,
    sceneMesh: accepted.sceneMesh,
    ...(accepted.compactScene ? { compactScene: accepted.compactScene } : {}),
    traceId: failed.traceId ?? accepted.traceId,
    diagnostics: {
      ...accepted.diagnostics,
      status: "error",
      file: failed.diagnostics.file,
      diagnostics: failed.diagnostics.diagnostics,
    },
  };
}
