import type { AddressInfo } from "node:net";
import type { EditHandle } from "./src/types/scene.ts";

export interface StudioHttpServer {
  readonly listening?: boolean;
  listen(port: number, hostname?: string, listeningListener?: () => void): this;
  address(): AddressInfo | string | null;
  close(callback?: (err?: Error) => void): this;
  registerStudioSession?(seed: unknown): void;
}

export interface StudioTraceLogEntry {
  readonly trace_id: string;
  readonly stage: "upload" | "inspect-asset" | "preview" | "compile" | "export";
  readonly asset: string | null;
  readonly format: string | null;
  readonly export_kind: string | null;
  readonly status: "ok" | "error";
  readonly diagnostic_code: string | null;
}

export interface StudioServerOptions {
  readonly workspaceRoot?: string;
  readonly sessions?: readonly unknown[];
  readonly projectCliRunner?: (args: readonly string[], json: boolean) => Promise<unknown>;
  readonly mcpToolRunner?: (options: {
    readonly workspaceRoot: string;
    readonly projectPath: string;
    readonly calls: readonly { readonly name: string; readonly arguments: Record<string, unknown> }[];
  }) => Promise<unknown[]>;
  readonly agentCliRunner?: (
    pyBin: string,
    args: readonly string[],
    options: {
      readonly env: Record<string, string | undefined>;
      readonly cwd: string;
      readonly signal?: AbortSignal | null;
      readonly onProgressLine?: (line: string) => void;
    },
  ) => Promise<{
    readonly stdout?: string;
    readonly stderr?: string;
    readonly exitCode?: number;
    readonly aborted?: boolean;
    readonly enoent?: boolean;
  }>;
  readonly cadCliRunner?: (
    cliBin: string,
    args: readonly string[],
    options?: { readonly maxBuffer?: number; readonly timeout?: number; readonly signal?: AbortSignal },
  ) => Promise<{ readonly stdout?: string; readonly stderr?: string }>;
  readonly onTraceLog?: (entry: StudioTraceLogEntry, line: string) => void;
}

export interface AgentLlmOptions {
  readonly provider?: string;
  readonly model?: string;
  readonly base_url?: string;
  readonly api_key?: string;
  readonly enable_vlm?: boolean;
  readonly few_shot_k?: number;
}

export interface AgentSummary {
  readonly passed: boolean | undefined;
  readonly archetype: string | null | undefined;
  readonly iterations_run: number | undefined;
  readonly contract: unknown;
  readonly verdict: unknown;
  readonly mode: string;
  readonly llm_provider: string | null;
  readonly llm_model: string | null;
  readonly used_llm: boolean;
  readonly vlm_enabled: boolean;
  readonly retrieved_examples: readonly string[];
  readonly contract_repair_attempts: number;
}

export function createStudioServer(options?: StudioServerOptions): StudioHttpServer;
export function registerStudioSession(server: StudioHttpServer, seed: unknown): void;
export function enrichEditHandles(
  source: string,
  handles: readonly EditHandle[],
): EditHandle[];
export function resolveSubprocessLlmEnv(
  baseEnv: Readonly<Record<string, string | undefined>>,
  provider: string,
  llm?: AgentLlmOptions,
): {
  readonly resolvedProvider: string;
  readonly env: Record<string, string | undefined>;
};
export function buildAgentCliArgs(options: {
  readonly prompt: string;
  readonly material: string;
  readonly manufacturingMethod: string;
  readonly mode: string;
  readonly resolvedProvider: string;
  readonly workDir: string;
  readonly cliBin: string;
  readonly workspaceRoot: string;
  readonly projectPath?: string;
  readonly target?: string;
  readonly previewBranch?: string;
  readonly contextAsset?: string;
  readonly existingKclPath?: string;
  readonly llm?: AgentLlmOptions;
  readonly includeDrawings?: boolean;
  readonly progressStderr?: boolean;
}): string[];
export function parseAgentProgressLine(
  line: string,
): (Record<string, unknown> & {
  readonly event: "progress";
  readonly stage: string;
  readonly message: string;
}) | null;
export function buildAgentSummary(
  payload: Readonly<Record<string, unknown>> | null | undefined,
  fallbackMode?: string,
): AgentSummary;
