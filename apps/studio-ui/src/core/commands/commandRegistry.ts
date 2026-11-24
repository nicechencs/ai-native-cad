import type { NavigationPresetId, SelectionIntentRule } from "../viewport/navigationPreset.ts";
import type { FeatureDashboardKind } from "../modeling/featureDashboardModel.ts";

export type CommandCategory =
  | "文件"
  | "编辑"
  | "建模"
  | "草图"
  | "装配"
  | "分析"
  | "检查"
  | "标注"
  | "视图"
  | "工具"
  | "AI"
  | "窗口"
  | "帮助";

export type ShortcutScope = "global" | "viewport";

export interface CommandContext {
  readonly hasSelection: boolean;
  readonly selectionKind: "face" | "edge" | null;
  readonly selectionCount?: number;
  readonly viewportTab: "3d" | "drawing";
  readonly isAssembly: boolean;
  readonly canExport: boolean;
  readonly exporting: boolean;
  readonly isHostBackend: boolean;
  readonly aiGenerating: boolean;
  readonly canUndo: boolean;
  readonly canRedo: boolean;
  readonly hasMeasurement: boolean;
  readonly showKeepInBox: boolean;
  readonly sectionCutEnabled: boolean;
  readonly explodeActive: boolean;
  readonly leftOpen: boolean;
  readonly rightOpen: boolean;
  readonly zenMode: boolean;
  readonly theme: "dark" | "light";
  readonly measurementMode: "circle" | "face" | null;
  readonly visualStyle?: "shaded-edges" | "shaded" | "translucent" | "wireframe" | "hlr";
  readonly projection?: "orthographic" | "perspective";
  readonly selectionFilter?: "all" | "face" | "edge" | "body";
  readonly selectionIntent?: SelectionIntentRule;
  readonly navigationPreset?: NavigationPresetId;
  readonly activeDashboardKind?: FeatureDashboardKind | null;
  readonly sketchActive?: boolean;
  readonly hasDisplayOverrides?: boolean;
  readonly isIsolated?: boolean;
  readonly hasPreview?: boolean;
}

export interface CommandDefinition {
  readonly id: string;
  readonly title: string;
  readonly category: CommandCategory;
  /** NX Command Finder style ribbon breadcrumb (e.g. "建模 › 工程修饰 › 孔"). */
  readonly ribbonPath?: string;
  readonly description?: string;
  readonly keywords?: readonly string[];
  readonly aliases?: readonly string[];
  readonly icon?: string;
  readonly shortcut?: string;
  readonly scope?: ShortcutScope;
  readonly when?: (ctx: CommandContext) => boolean;
  readonly disabledReason?: (ctx: CommandContext) => string;
  readonly isActive?: (ctx: CommandContext) => boolean;
  readonly execute: (args?: unknown) => void | Promise<void>;
}

export interface CommandExecutionResult {
  readonly ok: boolean;
  readonly reason?: string;
}

export function isCommandEnabled(cmd: CommandDefinition, ctx: CommandContext): boolean {
  return cmd.when ? cmd.when(ctx) : true;
}

export function searchCommands(
  commands: readonly CommandDefinition[],
  query: string,
  ctx: CommandContext,
): readonly CommandDefinition[] {
  const normalized = query.trim().toLowerCase();
  if (!normalized) {
    return commands;
  }
  const tokens = normalized.split(/\s+/);
  const scored: { readonly cmd: CommandDefinition; readonly score: number }[] = [];

  for (const cmd of commands) {
    const haystack = [
      cmd.title,
      cmd.category,
      cmd.ribbonPath ?? "",
      cmd.id,
      cmd.description ?? "",
      ...(cmd.keywords ?? []),
      ...(cmd.aliases ?? []),
    ]
      .join(" ")
      .toLowerCase();

    if (!tokens.every((token) => haystack.includes(token))) {
      continue;
    }

    let score = 0;
    const titleLower = cmd.title.toLowerCase();
    if (titleLower === normalized) score += 100;
    else if (titleLower.startsWith(normalized)) score += 60;
    else if (titleLower.includes(normalized)) score += 40;
    if (cmd.aliases?.some((alias) => alias.toLowerCase() === normalized)) score += 80;
    if (isCommandEnabled(cmd, ctx)) score += 10;
    scored.push({ cmd, score });
  }

  return scored
    .sort((a, b) => b.score - a.score || a.cmd.title.localeCompare(b.cmd.title, "zh-CN"))
    .map((entry) => entry.cmd);
}

export interface NormalizedShortcut {
  readonly ctrlOrMeta: boolean;
  readonly shift: boolean;
  readonly alt: boolean;
  readonly key: string;
}

export function parseShortcut(shortcut: string): NormalizedShortcut {
  const parts = shortcut.split("+").map((part) => part.trim());
  let ctrlOrMeta = false;
  let shift = false;
  let alt = false;
  let key = "";

  for (const part of parts) {
    const lower = part.toLowerCase();
    if (lower === "ctrl" || lower === "cmd" || lower === "meta" || lower === "mod") {
      ctrlOrMeta = true;
    } else if (lower === "shift") {
      shift = true;
    } else if (lower === "alt") {
      alt = true;
    } else {
      key = lower;
    }
  }
  return { ctrlOrMeta, shift, alt, key };
}

export function matchesKeyboardEvent(
  shortcut: string,
  event: {
    readonly key: string;
    readonly ctrlKey: boolean;
    readonly metaKey: boolean;
    readonly shiftKey: boolean;
    readonly altKey: boolean;
  },
): boolean {
  const parsed = parseShortcut(shortcut);
  const hasMod = event.ctrlKey || event.metaKey;
  if (parsed.ctrlOrMeta !== hasMod) return false;
  if (parsed.shift !== event.shiftKey) return false;
  if (parsed.alt !== event.altKey) return false;

  const eventKey = event.key.toLowerCase();
  if (parsed.key === "esc" || parsed.key === "escape") {
    return eventKey === "escape";
  }
  if (parsed.key === "space") {
    return eventKey === " " || eventKey === "spacebar";
  }
  return eventKey === parsed.key;
}
