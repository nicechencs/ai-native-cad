/**
 * Studio orbit yaw, shared by the viewport camera, the view cube, and the
 * named turn commands.
 *
 * Yaw 0 looks from -Y toward +Y. +X is screen-right and +Z is up, matching
 * the front drawing. A larger `depth` is farther from the camera.
 *
 * The right view looks from +X. That is the same eye direction as
 * `ViewKind::Right` (`toward_eye = +X`) and the drawing projection
 * `u = Y`, so +Y stays screen-right. The left view is the opposite eye.
 */

export const FRONT_YAW = 0;
export const RIGHT_YAW = -Math.PI / 2;
export const LEFT_YAW = Math.PI / 2;
export const BACK_YAW = Math.PI;
export const SIDE_VIEW_PITCH = 0.02;

/** One named turn from the front view. */
export const TURN_RIGHT_YAW = RIGHT_YAW;
export const TURN_LEFT_YAW = LEFT_YAW;

/** Horizontal drag of the view cube, in yaw radians per pixel. */
export const ORBIT_YAW_PER_PIXEL = -0.015;

export interface OrthographicOffset {
  readonly screenX: number;
  readonly screenUp: number;
  readonly depth: number;
}

export function orthographicOffset(
  yaw: number,
  pitch: number,
  dx: number,
  dy: number,
  dz: number,
): OrthographicOffset {
  const cosY = Math.cos(yaw);
  const sinY = Math.sin(yaw);
  const cosP = Math.cos(pitch);
  const sinP = Math.sin(pitch);
  const x1 = dx * cosY - dy * sinY;
  const y1 = dx * sinY + dy * cosY;
  return {
    screenX: x1,
    screenUp: y1 * sinP + dz * cosP,
    depth: y1 * cosP - dz * sinP,
  };
}

const CUBE_FACES = [
  ["right", 1, 0, 0],
  ["left", -1, 0, 0],
  ["back", 0, 1, 0],
  ["front", 0, -1, 0],
  ["top", 0, 0, 1],
  ["bottom", 0, 0, -1],
] as const;

export type CubeFace = (typeof CUBE_FACES)[number][0];

/** The cube face whose outward normal points toward the camera. */
export function nearestCubeFace(yaw: number, pitch: number): CubeFace {
  let best: CubeFace = "front";
  let bestDepth = Number.POSITIVE_INFINITY;
  for (const [name, x, y, z] of CUBE_FACES) {
    const depth = orthographicOffset(yaw, pitch, x, y, z).depth;
    if (depth < bestDepth) {
      bestDepth = depth;
      best = name;
    }
  }
  return best;
}

/** Yaw that puts the camera on the outside of an outward face normal. */
export function yawForOutwardNormal(nx: number, ny: number): number {
  return Math.atan2(-nx, -ny);
}
