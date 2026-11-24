/**
 * Experimental SubD, TPMS, and GPU tracks stay out of the mechanical workbench
 * until an explicit flag is turned on.
 *
 * Enable with `localStorage["aicad.experimentalTracks"] = "1"`
 * or `?experimental=1`.
 */
import type { BlobKind, IrNode } from "../../types/graphIr.ts";

export const EXPERIMENTAL_TRACKS_STORAGE_KEY = "aicad.experimentalTracks";

export function readExperimentalTracksFlag(source: {
  readonly storageValue?: string | null;
  readonly search?: string;
}): boolean {
  const stored = source.storageValue?.trim().toLowerCase();
  if (stored === "1" || stored === "true") return true;
  const query = new URLSearchParams(source.search ?? "").get("experimental");
  const flag = query?.trim().toLowerCase();
  return flag === "1" || flag === "true";
}

export function experimentalTracksEnabled(): boolean {
  let storageValue: string | null = null;
  try {
    storageValue = globalThis.localStorage?.getItem(EXPERIMENTAL_TRACKS_STORAGE_KEY) ?? null;
  } catch {
    storageValue = null;
  }
  const search = typeof window !== "undefined" ? window.location.search : "";
  return readExperimentalTracksFlag({ storageValue, search });
}

/** SubD cages, TPMS fields, and implicit-field nodes are not part of the mechanical default. */
export function isExperimentalFeatureNode(node: IrNode): boolean {
  const kind = node.kind.kind;
  if (
    kind === "field_tpms"
    || kind.startsWith("field_")
    || kind.startsWith("implicit_")
    || kind.startsWith("subd_")
  ) {
    return true;
  }
  return node.output_type === "subd"
    || node.output_type === "scalar_field"
    || node.output_type === "vector_field";
}

export function isExperimentalBlob(kind: BlobKind): boolean {
  return kind === "subd" || kind === "field";
}
