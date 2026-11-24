/**
 * Point-and-Prompt request builder.
 * Selected faces, edges, and features travel as structured ids. The prompt text stays as the user typed it.
 */
import type { AiGenerateMode, AiLlmConfig, SelectionTarget } from "../../types/scene.ts";

export interface PointAndPromptPayload {
  readonly prompt: string;
  readonly selectedTopoRefs: readonly string[];
  readonly selectedNodeIds: readonly string[];
}

export function buildPointAndPromptPayload(input: {
  readonly prompt: string;
  readonly attachSelection: boolean;
  readonly selectedTarget?: SelectionTarget;
  readonly selectedTopoRefs?: readonly string[];
  readonly selectedNodeIds?: readonly string[];
}): PointAndPromptPayload {
  const prompt = input.prompt.trim();
  if (!input.attachSelection) {
    return { prompt, selectedTopoRefs: [], selectedNodeIds: [] };
  }
  const topoRefs = uniqueStrings(input.selectedTopoRefs ?? []);
  const nodeIds = uniqueStrings(input.selectedNodeIds ?? []);
  const target = input.selectedTarget;
  if (target?.kind === "face") topoRefs.push(...uniqueStrings([target.face.face_id]).filter((id) => !topoRefs.includes(id)));
  if (target?.kind === "edge") topoRefs.push(...uniqueStrings([target.edge.edge_id]).filter((id) => !topoRefs.includes(id)));
  return { prompt, selectedTopoRefs: topoRefs, selectedNodeIds: nodeIds };
}

export function buildAiGenerateRequest(input: {
  readonly prompt: string;
  readonly material: string;
  readonly manufacturingMethod: string;
  readonly mode: AiGenerateMode;
  readonly existingSource?: string;
  readonly projectPath?: string | null;
  readonly sessionId?: string | null;
  readonly llm?: AiLlmConfig;
  readonly selectedTopoRefs?: readonly string[];
  readonly selectedNodeIds?: readonly string[];
}): {
  readonly prompt: string;
  readonly material: string;
  readonly manufacturing_method: string;
  readonly mode: AiGenerateMode;
  readonly existing_source: string | undefined;
  readonly projectPath?: string;
  readonly sessionId?: string;
  readonly llm: AiLlmConfig | undefined;
  readonly selectedTopoRefs: readonly string[];
  readonly selectedNodeIds: readonly string[];
} {
  const projectPath = typeof input.projectPath === "string" && input.projectPath.trim()
    ? input.projectPath.trim()
    : undefined;
  const sessionId = typeof input.sessionId === "string" && input.sessionId.trim()
    ? input.sessionId.trim()
    : undefined;
  return {
    prompt: input.prompt,
    material: input.material,
    manufacturing_method: input.manufacturingMethod,
    mode: input.mode,
    existing_source: input.mode === "edit" ? input.existingSource : undefined,
    ...(projectPath ? { projectPath } : {}),
    ...(sessionId ? { sessionId } : {}),
    llm: input.llm
      ? {
          ...input.llm,
          ...(input.llm.enable_vlm !== undefined ? { enable_vlm: input.llm.enable_vlm } : {}),
          ...(input.llm.few_shot_k !== undefined ? { few_shot_k: input.llm.few_shot_k } : {}),
        }
      : undefined,
    selectedTopoRefs: [...(input.selectedTopoRefs ?? [])],
    selectedNodeIds: [...(input.selectedNodeIds ?? [])],
  };
}

function uniqueStrings(values: readonly string[]): string[] {
  const seen = new Set<string>();
  const ordered: string[] = [];
  for (const value of values) {
    const trimmed = value.trim();
    if (!trimmed || seen.has(trimmed)) continue;
    seen.add(trimmed);
    ordered.push(trimmed);
  }
  return ordered;
}
