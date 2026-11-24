import type {
  AiGenerateMode,
  AiProgressStep,
  AiRunFeedback,
  GeometryMetrics,
} from "../../types/scene.ts";

export interface DiffLine {
  readonly kind: "context" | "add" | "remove";
  readonly oldLine?: number;
  readonly newLine?: number;
  readonly text: string;
}

export interface KclDiffSummary {
  readonly added: number;
  readonly removed: number;
  readonly lines: readonly DiffLine[];
  readonly truncated: boolean;
}

export interface AiThreadItem {
  readonly id: string;
  readonly createdAt: number;
  readonly mode: AiGenerateMode;
  readonly prompt: string;
  readonly material: string;
  readonly method: string;
  readonly contextLabel?: string;
  readonly modelLabel: string;
  readonly status: "running" | "success" | "error" | "cancelled";
  readonly steps: readonly AiProgressStep[];
  readonly feedback?: AiRunFeedback;
  readonly undoRevision?: number;
}

export interface AiStarterPrompt {
  readonly id: string;
  readonly title: string;
  readonly tag: string;
  readonly mode: AiGenerateMode;
  readonly prompt: string;
}

export const AI_STARTER_PROMPTS: readonly AiStarterPrompt[] = [
  {
    id: "nema17",
    title: "NEMA17 电机安装支架",
    tag: "CNC 3轴",
    mode: "generate",
    prompt: "设计一个 NEMA17 步进电机 L 型安装支架，带 22mm 止口定位孔、4 个 M3 电机固定孔与底板 4 个 M4 腰形安装孔，内侧带 6mm 加强筋。",
  },
  {
    id: "enclosure",
    title: "薄壁铝合金控制盒",
    tag: "壳体",
    mode: "generate",
    prompt: "设计一个 120x80x35mm 的铝合金薄壁控制盒，壁厚 3mm，四角带 M4 螺纹固定柱，顶部边缘 R3 倒圆角。",
  },
  {
    id: "flange",
    title: "轻量化法兰轴承座",
    tag: "轴系",
    mode: "generate",
    prompt: "设计一个带 Ø28mm 轴承内孔的圆形法兰座，外径 84mm，厚度 14mm，圆周均布 6 个 M5 沉头孔。",
  },
];

export const AI_QUICK_EDIT_CHIPS: readonly { readonly label: string; readonly text: string }[] = [
  { label: "外缘 R2 倒圆角", text: "给零件主要外轮廓边添加 R2mm 倒圆角，避免应力集中" },
  { label: "4×M5 沉头孔", text: "将安装孔修改为 4 个 M5 沉头孔（沉孔 Ø9.5mm 深 5.5mm）" },
  { label: "底板增厚 +4mm", text: "将底板厚度增加 4mm，保持上表面安装孔贯穿" },
  { label: "轻量化减重槽", text: "在非承力区域增加轻量化减重口袋，保持最小壁厚 ≥ 3mm" },
];

const MAX_DIFF_MATRIX_CELLS = 160_000;

export function computeKclLineDiff(before: string, after: string): KclDiffSummary {
  const a = before.replace(/\r\n/g, "\n").split("\n");
  const b = after.replace(/\r\n/g, "\n").split("\n");

  if (a.length * b.length > MAX_DIFF_MATRIX_CELLS) {
    const lines: DiffLine[] = [
      ...a.slice(0, 60).map((text, idx) => ({ kind: "remove" as const, oldLine: idx + 1, text })),
      ...b.slice(0, 60).map((text, idx) => ({ kind: "add" as const, newLine: idx + 1, text })),
    ];
    return { added: b.length, removed: a.length, lines, truncated: true };
  }

  const m = a.length;
  const n = b.length;
  const dp: Uint32Array[] = Array.from({ length: m + 1 }, () => new Uint32Array(n + 1));

  for (let i = m - 1; i >= 0; i--) {
    for (let j = n - 1; j >= 0; j--) {
      if (a[i] === b[j]) {
        dp[i][j] = dp[i + 1][j + 1] + 1;
      } else {
        dp[i][j] = dp[i + 1][j] >= dp[i][j + 1] ? dp[i + 1][j] : dp[i][j + 1];
      }
    }
  }

  const allLines: DiffLine[] = [];
  let i = 0;
  let j = 0;
  let added = 0;
  let removed = 0;

  while (i < m && j < n) {
    if (a[i] === b[j]) {
      allLines.push({ kind: "context", oldLine: i + 1, newLine: j + 1, text: a[i] });
      i++;
      j++;
    } else if (dp[i + 1][j] >= dp[i][j + 1]) {
      allLines.push({ kind: "remove", oldLine: i + 1, text: a[i] });
      removed++;
      i++;
    } else {
      allLines.push({ kind: "add", newLine: j + 1, text: b[j] });
      added++;
      j++;
    }
  }
  while (i < m) {
    allLines.push({ kind: "remove", oldLine: i + 1, text: a[i] });
    removed++;
    i++;
  }
  while (j < n) {
    allLines.push({ kind: "add", newLine: j + 1, text: b[j] });
    added++;
    j++;
  }

  // Keep changed lines plus 2 lines of surrounding context for readability.
  const keep = new Uint8Array(allLines.length);
  for (let idx = 0; idx < allLines.length; idx++) {
    if (allLines[idx].kind !== "context") {
      for (let k = Math.max(0, idx - 2); k <= Math.min(allLines.length - 1, idx + 2); k++) {
        keep[k] = 1;
      }
    }
  }

  const compact = allLines.filter((_line, idx) => keep[idx] === 1);
  const truncated = compact.length > 140;
  return {
    added,
    removed,
    lines: truncated ? compact.slice(0, 140) : compact,
    truncated,
  };
}

export interface MetricsDeltaSummary {
  readonly volumeBeforeCm3: number;
  readonly volumeAfterCm3: number;
  readonly volumeDeltaPct: number | null;
  readonly facesBefore: number;
  readonly facesAfter: number;
  readonly watertightAfter: boolean;
}

export function computeMetricsDelta(
  before: GeometryMetrics | undefined,
  after: GeometryMetrics | undefined,
): MetricsDeltaSummary | null {
  if (!after || after.face_count <= 0) return null;
  const volBefore = before ? before.volume_mm3 / 1000 : 0;
  const volAfter = after.volume_mm3 / 1000;
  const pct =
    before && before.volume_mm3 > 1e-6
      ? ((after.volume_mm3 - before.volume_mm3) / before.volume_mm3) * 100
      : null;
  return {
    volumeBeforeCm3: volBefore,
    volumeAfterCm3: volAfter,
    volumeDeltaPct: pct,
    facesBefore: before?.face_count ?? 0,
    facesAfter: after.face_count,
    watertightAfter: after.watertight,
  };
}

export function formatDurationMs(ms: number | undefined): string {
  if (typeof ms !== "number" || !Number.isFinite(ms) || ms < 0) return "";
  if (ms < 1000) return `${Math.round(ms)}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}
