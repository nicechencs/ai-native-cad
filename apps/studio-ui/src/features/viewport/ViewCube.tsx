import { useMemo, useRef, type PointerEvent as ReactPointerEvent } from "react";
import type { ProjectionMode, ViewPreset } from "../../core/viewport/displayState.ts";
import { CadIcon } from "../../ui/icons/CadIcons.tsx";
import { ORBIT_YAW_PER_PIXEL, TURN_LEFT_YAW, TURN_RIGHT_YAW } from "./viewOrientation.ts";

export interface ViewCubeProps {
  readonly yaw: number;
  readonly pitch: number;
  readonly projection: ProjectionMode;
  readonly onApplyPreset: (preset: ViewPreset) => void;
  readonly onFitView: () => void;
  readonly onToggleProjection: () => void;
  readonly onRotateStep?: (deltaYaw: number) => void;
  readonly onOrbitBy?: (deltaYaw: number, deltaPitch: number) => void;
}

interface CubeFaceDef {
  readonly preset: ViewPreset;
  readonly label: string;
  readonly shortLabel: string;
  readonly normal: readonly [number, number, number];
  readonly corners: readonly [
    readonly [number, number, number],
    readonly [number, number, number],
    readonly [number, number, number],
    readonly [number, number, number],
  ];
}

const CUBE_FACES: readonly CubeFaceDef[] = [
  {
    preset: "top",
    label: "俯视",
    shortLabel: "顶",
    normal: [0, 0, 1],
    corners: [[-1, -1, 1], [1, -1, 1], [1, 1, 1], [-1, 1, 1]],
  },
  {
    preset: "bottom",
    label: "仰视",
    shortLabel: "底",
    normal: [0, 0, -1],
    corners: [[-1, 1, -1], [1, 1, -1], [1, -1, -1], [-1, -1, -1]],
  },
  {
    preset: "front",
    label: "主视",
    shortLabel: "前",
    normal: [0, -1, 0],
    corners: [[-1, -1, -1], [1, -1, -1], [1, -1, 1], [-1, -1, 1]],
  },
  {
    preset: "back",
    label: "后视",
    shortLabel: "后",
    normal: [0, 1, 0],
    corners: [[1, 1, -1], [-1, 1, -1], [-1, 1, 1], [1, 1, 1]],
  },
  {
    preset: "right",
    label: "右视",
    shortLabel: "右",
    normal: [1, 0, 0],
    corners: [[1, -1, -1], [1, 1, -1], [1, 1, 1], [1, -1, 1]],
  },
  {
    preset: "left",
    label: "左视",
    shortLabel: "左",
    normal: [-1, 0, 0],
    corners: [[-1, 1, -1], [-1, -1, -1], [-1, -1, 1], [-1, 1, 1]],
  },
];

function rotatePoint(
  x: number,
  y: number,
  z: number,
  yaw: number,
  pitch: number,
): readonly [number, number, number] {
  const cosY = Math.cos(yaw);
  const sinY = Math.sin(yaw);
  const cosP = Math.cos(pitch);
  const sinP = Math.sin(pitch);

  const x1 = x * cosY - y * sinY;
  const y1 = x * sinY + y * cosY;
  const depth = y1 * cosP - z * sinP;
  const z2 = y1 * sinP + z * cosP;
  return [x1, -z2, depth];
}

export function ViewCube({
  yaw,
  pitch,
  projection,
  onApplyPreset,
  onFitView,
  onToggleProjection,
  onRotateStep,
  onOrbitBy,
}: ViewCubeProps) {
  const dragRef = useRef<{ active: boolean; moved: boolean; x: number; y: number }>({
    active: false,
    moved: false,
    x: 0,
    y: 0,
  });

  const handleSvgPointerDown = (event: ReactPointerEvent<SVGSVGElement>) => {
    if (!onOrbitBy || !event.isPrimary || event.button !== 0) return;
    event.currentTarget.setPointerCapture(event.pointerId);
    dragRef.current = { active: true, moved: false, x: event.clientX, y: event.clientY };
  };

  const handleSvgPointerMove = (event: ReactPointerEvent<SVGSVGElement>) => {
    if (!onOrbitBy || !dragRef.current.active) return;
    const dx = event.clientX - dragRef.current.x;
    const dy = event.clientY - dragRef.current.y;
    const moved = dragRef.current.moved || Math.abs(dx) > 3 || Math.abs(dy) > 3;
    dragRef.current = { active: true, moved, x: event.clientX, y: event.clientY };
    if (moved) {
      onOrbitBy(dx * ORBIT_YAW_PER_PIXEL, dy * 0.015);
    }
  };

  const handleSvgPointerUp = () => {
    dragRef.current.active = false;
  };

  const projectedFaces = useMemo(() => {
    const scale = 22;
    const cx = 42;
    const cy = 42;
    return CUBE_FACES.map((face) => {
      const [, , nz] = rotatePoint(face.normal[0], face.normal[1], face.normal[2], yaw, pitch);
      const pts = face.corners.map(([x, y, z]) => {
        const [px, py, d] = rotatePoint(x, y, z, yaw, pitch);
        return [cx + px * scale, cy + py * scale, d] as const;
      });
      const avgDepth = (pts[0][2] + pts[1][2] + pts[2][2] + pts[3][2]) / 4;
      const centerX = (pts[0][0] + pts[1][0] + pts[2][0] + pts[3][0]) / 4;
      const centerY = (pts[0][1] + pts[1][1] + pts[2][1] + pts[3][1]) / 4;
      return {
        face,
        // Front-facing faces in this camera basis have negative depth (camera looks toward +Y after rotation)
        visible: nz < 0.05,
        avgDepth,
        centerX,
        centerY,
        pointsAttr: pts.map(([x, y]) => `${x.toFixed(1)},${y.toFixed(1)}`).join(" "),
        shade: Math.max(0.15, Math.min(1, -nz)),
      };
    })
      .filter((item) => item.visible)
      .sort((a, b) => b.avgDepth - a.avgDepth);
  }, [yaw, pitch]);

  return (
    <div className="viewport-viewcube" role="group" aria-label="标准视角魔方">
      <div className="viewcube-stage">
        <button
          type="button"
          className="viewcube-home-btn"
          title="等轴测复位 (1)"
          aria-label="等轴测"
          onClick={() => onApplyPreset("iso")}
        >
          <CadIcon name="viewIso" size={12} />
        </button>
        <button
          type="button"
          className="viewcube-fit-btn"
          title="适应窗口 (0)"
          aria-label="适应窗口"
          onClick={onFitView}
        >
          <CadIcon name="fitView" size={12} />
        </button>
        {onRotateStep && (
          <>
            <button
              type="button"
              className="viewcube-rot-left-btn"
              title="视角左转 90°"
              aria-label="视角左转 90 度"
              onClick={() => onRotateStep(TURN_LEFT_YAW)}
            >
              <CadIcon name="rotateLeft" size={11} />
            </button>
            <button
              type="button"
              className="viewcube-rot-right-btn"
              title="视角右转 90°"
              aria-label="视角右转 90 度"
              onClick={() => onRotateStep(TURN_RIGHT_YAW)}
            >
              <CadIcon name="rotateRight" size={11} />
            </button>
          </>
        )}

        <svg
          className="viewcube-svg"
          width={84}
          height={84}
          viewBox="0 0 84 84"
          aria-label="交互式视角魔方"
          onPointerDown={handleSvgPointerDown}
          onPointerMove={handleSvgPointerMove}
          onPointerUp={handleSvgPointerUp}
          onPointerCancel={handleSvgPointerUp}
        >
          {projectedFaces.map(({ face, pointsAttr, centerX, centerY, shade }) => (
            <g
              key={face.preset}
              className={`viewcube-face is-${face.preset}`}
              role="button"
              tabIndex={0}
              aria-label={`${face.label}视角`}
              onClick={() => {
                if (dragRef.current.moved) {
                  dragRef.current.moved = false;
                  return;
                }
                onApplyPreset(face.preset);
              }}
              onKeyDown={(event) => {
                if (event.key === "Enter" || event.key === " ") {
                  event.preventDefault();
                  onApplyPreset(face.preset);
                }
              }}
            >
              <title>{`${face.label}视角（拖拽魔方可自由旋转）`}</title>
              <polygon
                points={pointsAttr}
                style={{
                  fillOpacity: Number((0.45 + shade * 0.48).toFixed(2)),
                }}
              />
              <text
                x={centerX}
                y={centerY}
                textAnchor="middle"
                dominantBaseline="central"
              >
                {face.shortLabel}
              </text>
            </g>
          ))}
        </svg>
      </div>

      <div className="viewcube-quick-row">
        {(
          [
            ["iso", "等轴", "1"],
            ["front", "前", "3"],
            ["back", "后", "6"],
            ["top", "顶", "2"],
            ["right", "右", "4"],
            ["left", "左", "5"],
          ] as const
        ).map(([preset, label, keyHint]) => (
          <button
            key={preset}
            type="button"
            className="viewcube-chip-btn"
            title={`${label}视 (${keyHint})`}
            onClick={() => onApplyPreset(preset)}
          >
            {label}
          </button>
        ))}
        <button
          type="button"
          className="viewcube-chip-btn is-proj"
          title={`切换投影模式 (P) · 当前：${projection === "orthographic" ? "正交" : "透视"}`}
          onClick={onToggleProjection}
        >
          {projection === "orthographic" ? "正交" : "透视"}
        </button>
      </div>
    </div>
  );
}
