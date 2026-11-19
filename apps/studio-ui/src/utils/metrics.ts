import type { BoundingBox3D } from "../types/scene.ts";

export interface EnvelopeStatus {
  readonly bboxSizeX: number;
  readonly bboxSizeY: number;
  readonly bboxSizeZ: number;
  readonly envelopeRatio: number;
  readonly barWidthPercent: number;
  readonly isExceeded: boolean;
}

/**
 * Computes Keep-In bounding box utilization without prematurely clamping
 * `envelopeRatio` so out-of-bounds states (> 100%) remain observable.
 */
export function computeEnvelopeStatus(
  actualBbox: BoundingBox3D,
  keepInBbox: BoundingBox3D,
): EnvelopeStatus {
  const bboxSizeX = actualBbox.max[0] - actualBbox.min[0];
  const bboxSizeY = actualBbox.max[1] - actualBbox.min[1];
  const bboxSizeZ = actualBbox.max[2] - actualBbox.min[2];

  const keepInSizeX = Math.max(1e-6, keepInBbox.max[0] - keepInBbox.min[0]);
  const keepInSizeY = Math.max(1e-6, keepInBbox.max[1] - keepInBbox.min[1]);
  const keepInSizeZ = Math.max(1e-6, keepInBbox.max[2] - keepInBbox.min[2]);

  const envelopeRatio = Math.round(
    Math.max(
      bboxSizeX / keepInSizeX,
      bboxSizeY / keepInSizeY,
      bboxSizeZ / keepInSizeZ,
    ) * 100,
  );

  const isBoundsViolated =
    actualBbox.min[0] < keepInBbox.min[0] ||
    actualBbox.min[1] < keepInBbox.min[1] ||
    actualBbox.min[2] < keepInBbox.min[2] ||
    actualBbox.max[0] > keepInBbox.max[0] ||
    actualBbox.max[1] > keepInBbox.max[1] ||
    actualBbox.max[2] > keepInBbox.max[2];

  return {
    bboxSizeX,
    bboxSizeY,
    bboxSizeZ,
    envelopeRatio,
    barWidthPercent: Math.min(100, Math.max(0, envelopeRatio)),
    isExceeded: envelopeRatio > 100 || isBoundsViolated,
  };
}
