import { createContext, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { WorkbenchUnavailableError, type WorkbenchBackend, type WorkspaceAsset } from "../sync/workbenchBackend.ts";
import { tokenizeKclLine, type KclTokenCategory } from "../language/kclMonarch.ts";
import { VirtualAssemblyTreePanel } from "../features/assembly-tree/VirtualAssemblyTreePanel.tsx";
import { ExplainModelPanel } from "../features/feature-tree/ExplainModelPanel.tsx";
import { FaultTolerantFeatureTree } from "../features/feature-tree/FaultTolerantFeatureTree.tsx";
import { ValidationPanel } from "../features/validation/ValidationPanel.tsx";
import type { DatumPlane } from "../types/apiCommands.ts";
import type { CadSessionTransport, RoiInterfaceDigest, VirtualTreeNode } from "../sync/cadSessionTransport.ts";
import { graphIrForDocument } from "../features/feature-tree/graphPatchBuilder.ts";
import type { CacheStatus, CanonicalGraph, GraphPatch, ParameterDef } from "../types/graphIr.ts";
import type { VerificationReport } from "../types/verification.ts";
import { INITIAL_DEMO_KCL } from "../data/demoWorkbenchState.ts";
import type {
  AiConfigStatus,
  AiGenerateMode,
  AiLlmConfig,
  AiProgressStep,
  AiProvider,
  AiRequestOptions,
  AiRunFeedback,
  AssetInspectionSummary,
  CompilerDiagnostic,
  DesignContract,
  DiagnosticsReport,
  LspCompletionItem,
  LspHoverInfo,
  SceneMeshPayload,
  SelectionTarget,
  SourceSpan,
} from "../types/scene.ts";
import { localStepAsset, rememberBrowserStepFile } from "../preview/stepPreview.ts";
import { analyzeKclLspFallback, applyKclCompletion } from "../utils/studioHelpers.ts";
import { loadAiConfig, saveAiConfig, type StoredAiConfig } from "../features/ai-assistant/aiConfigStore.ts";
import { loadWorkbenchUiPrefs, updateWorkbenchUiPrefs } from "../core/layout/workbenchPrefsStore.ts";
import {
  countHiddenEntities,
  countTranslucentEntities,
  hasActiveDisplayOverrides,
  type ViewportDisplayState,
} from "../core/viewport/displayState.ts";
import { CadIcon } from "../ui/icons/CadIcons.tsx";
import { TraceIdChip } from "./TraceIdChip.tsx";
import { traceIdFrom } from "../utils/trace.ts";
import { focusWorkbenchTab, WorkbenchTabPanel, WorkbenchTabs } from "./WorkbenchTabs.tsx";

const LEFT_TABS = [
  { id: "model", label: "模型树" },
  { id: "assembly", label: "装配树" },
  { id: "source", label: "源码" },
  { id: "assets", label: "资产" },
  { id: "checks", label: "诊断" },
  { id: "ai", label: "AI" },
] as const;

export type LeftWorkbenchTab = (typeof LEFT_TABS)[number]["id"];

export interface LeftPanelProps {
  readonly activeTab: LeftWorkbenchTab;
  /**
   * The activity bar still owns the original tabs.
   * `assembly` is selected inside this panel when it draws its own tab strip.
   */
  readonly onSelectTab: (tab: Exclude<LeftWorkbenchTab, "assembly">) => void;
  readonly embedded?: boolean;
  readonly backend: WorkbenchBackend;
  readonly aiGenMode: AiGenerateMode;
  readonly onSelectAiMode: (mode: AiGenerateMode) => void;
  readonly sceneMesh: SceneMeshPayload;
  readonly selectedTarget: SelectionTarget;
  readonly onSelectTarget: (target: NonNullable<SelectionTarget>) => void;
  readonly contract: DesignContract | null;
  readonly verificationReport: VerificationReport;
  readonly diagnosticsReport: DiagnosticsReport;
  /** Trace id for the latest compile, shown when source diagnostics failed. */
  readonly traceId?: string;
  readonly kclSource: string;
  readonly highlightedSpan: SourceSpan | null;
  readonly highlightLabel: string | null;
  readonly onChangeSource: (nextSource: string) => void;
  readonly onSelectSpan: (span: SourceSpan, label: string) => void;
  readonly onAiGenerate?: (
    prompt: string,
    material: string,
    manufacturingMethod: string,
    options?: AiRequestOptions,
  ) => void | Promise<void>;
  readonly aiConfigStatus?: AiConfigStatus | null;
  readonly aiGenerating?: boolean;
  readonly aiFeedback?: AiRunFeedback | null;
  readonly aiProgressSteps?: readonly AiProgressStep[];
  /** Live canonical graph. When omitted, the feature-tree section stays hidden. */
  readonly graphIr?: CanonicalGraph;
  readonly graphCacheStatus?: CacheStatus;
  readonly selectedGraphNodeId?: string | null;
  readonly onEmitGraphPatch?: (patch: GraphPatch) => void;
  readonly onSelectGraphNode?: (nodeId: string, failedTopoRefs: readonly string[]) => void;
  /** Workspace `.aicad` path. Null means the project is still only in memory. */
  readonly activeProjectPath?: string | null;
  /** Create a workspace `.aicad` project. Offline servers should resolve locally instead of throwing. */
  readonly onInitRemoteProject?: (relativeDir: string, name: string) => void | Promise<void>;
  /** Write one feature-graph patch to the active `.aicad` project. */
  readonly onSyncRemotePatch?: (patch: GraphPatch) => void | Promise<void>;
  /** Host session door. Omitted until App (or a test) mounts one. */
  readonly sessionTransport?: CadSessionTransport;
  /** Host session for the virtual assembly tree. Null shows the empty prompt. */
  readonly assemblySessionId?: string | null;
  readonly onSelectAssemblyInstance?: (node: VirtualTreeNode) => void;
  readonly onAssemblyDigestReady?: (digest: RoiInterfaceDigest) => void;
  /** Latest assembly status line from the notification log. */
  readonly assemblyNotice?: string;
  /** Optional 3D display layering state & Display Pane callbacks. */
  readonly displayState?: ViewportDisplayState;
  readonly selectedComponentId?: string | null;
  readonly onSelectComponent?: (componentId: string | null) => void;
  readonly onToggleEntityHidden?: (scope: "component" | "face", id: string) => void;
  readonly onToggleEntityTranslucent?: (scope: "component" | "face", id: string) => void;
  readonly onIsolateEntity?: (id: string) => void;
  readonly onShowAllEntities?: () => void;
  readonly onStartSketchOnDatum?: (plane: DatumPlane) => void;
  readonly onAiRepairNode?: (nodeId: string, reason: string, failedTopoRefs: readonly string[]) => void;
}

/** State App keeps for the feature tree mounted inside `LeftPanel`. */
export interface FeatureTreeMountValue {
  readonly graphIr?: CanonicalGraph;
  readonly graphCacheStatus?: CacheStatus;
  readonly selectedGraphNodeId?: string | null;
  /** Topology refs recorded by the latest node selection, for viewport rollback highlight. */
  readonly highlightedTopoRefs?: readonly string[];
  /** Same selection as `highlightedTopoRefs`. The viewport badge reads this name. */
  readonly highlightedFailedTopoRefs?: readonly string[];
  /** Latest concise notice from the notification log. */
  readonly patchNotice?: string;
  /** Trace id for the latest feature-tree error notice. */
  readonly patchTraceId?: string;
  readonly onEmitGraphPatch?: (patch: GraphPatch) => void;
  readonly onSelectGraphNode?: (nodeId: string, failedTopoRefs: readonly string[]) => void;
  readonly onRenameNode?: (nodeId: string, name: string) => void;
  readonly onSetNodeSuppressed?: (nodeId: string, suppressed: boolean) => void;
  readonly onUpsertParameter?: (value: ParameterDef) => void;
  readonly onJumpToViolation?: (target: { readonly nodeId: string | null; readonly topoRef: string | null }) => void;
  readonly activeProjectPath?: string | null;
  readonly onInitRemoteProject?: (relativeDir: string, name: string) => void | Promise<void>;
  readonly onSyncRemotePatch?: (patch: GraphPatch) => void | Promise<void>;
}

const FeatureTreeMountContext = createContext<FeatureTreeMountValue | null>(null);

export function FeatureTreeMountProvider({
  value,
  children,
}: {
  readonly value: FeatureTreeMountValue;
  readonly children: ReactNode;
}) {
  return <FeatureTreeMountContext.Provider value={value}>{children}</FeatureTreeMountContext.Provider>;
}

function resolveFeatureTreeMount(
  props: FeatureTreeMountValue,
  mounted: FeatureTreeMountValue | null,
  source: string,
  demoSource: string,
): FeatureTreeMountValue {
  return {
    graphIr: graphIrForDocument(source, demoSource, props.graphIr, mounted?.graphIr),
    graphCacheStatus: props.graphCacheStatus ?? mounted?.graphCacheStatus,
    selectedGraphNodeId: props.selectedGraphNodeId !== undefined
      ? props.selectedGraphNodeId
      : mounted?.selectedGraphNodeId,
    highlightedTopoRefs: props.highlightedTopoRefs ?? mounted?.highlightedTopoRefs,
    highlightedFailedTopoRefs: props.highlightedFailedTopoRefs ?? mounted?.highlightedFailedTopoRefs,
    patchNotice: props.patchNotice ?? mounted?.patchNotice,
    patchTraceId: props.patchTraceId ?? mounted?.patchTraceId,
    onEmitGraphPatch: props.onEmitGraphPatch ?? mounted?.onEmitGraphPatch,
    onSelectGraphNode: props.onSelectGraphNode ?? mounted?.onSelectGraphNode,
    onRenameNode: props.onRenameNode ?? mounted?.onRenameNode,
    onSetNodeSuppressed: props.onSetNodeSuppressed ?? mounted?.onSetNodeSuppressed,
    onUpsertParameter: props.onUpsertParameter ?? mounted?.onUpsertParameter,
    onJumpToViolation: props.onJumpToViolation ?? mounted?.onJumpToViolation,
    activeProjectPath: props.activeProjectPath !== undefined
      ? props.activeProjectPath
      : mounted?.activeProjectPath,
    onInitRemoteProject: props.onInitRemoteProject ?? mounted?.onInitRemoteProject,
    onSyncRemotePatch: props.onSyncRemotePatch ?? mounted?.onSyncRemotePatch,
  };
}

export function useFeatureTreeMount(): FeatureTreeMountValue | null {
  return useContext(FeatureTreeMountContext);
}

/** State App keeps for the virtual assembly tree mounted inside `LeftPanel`. */
export interface AssemblyTreeMountValue {
  readonly sessionTransport?: CadSessionTransport;
  readonly assemblySessionId?: string | null;
  readonly onSelectAssemblyInstance?: (node: VirtualTreeNode) => void;
  readonly onAssemblyDigestReady?: (digest: RoiInterfaceDigest) => void;
  readonly assemblyNotice?: string;
  /** Trace id for the latest assembly error notice. */
  readonly assemblyTraceId?: string;
}

const AssemblyTreeMountContext = createContext<AssemblyTreeMountValue | null>(null);

export function AssemblyTreeMountProvider({
  value,
  children,
}: {
  readonly value: AssemblyTreeMountValue;
  readonly children: ReactNode;
}) {
  return <AssemblyTreeMountContext.Provider value={value}>{children}</AssemblyTreeMountContext.Provider>;
}

function resolveAssemblyTreeMount(
  props: AssemblyTreeMountValue,
  mounted: AssemblyTreeMountValue | null,
): AssemblyTreeMountValue {
  return {
    sessionTransport: props.sessionTransport ?? mounted?.sessionTransport,
    assemblySessionId: props.assemblySessionId !== undefined
      ? props.assemblySessionId
      : mounted?.assemblySessionId,
    onSelectAssemblyInstance: props.onSelectAssemblyInstance ?? mounted?.onSelectAssemblyInstance,
    onAssemblyDigestReady: props.onAssemblyDigestReady ?? mounted?.onAssemblyDigestReady,
    assemblyNotice: props.assemblyNotice ?? mounted?.assemblyNotice,
    assemblyTraceId: props.assemblyTraceId ?? mounted?.assemblyTraceId,
  };
}

export function useAssemblyTreeMount(): AssemblyTreeMountValue | null {
  return useContext(AssemblyTreeMountContext);
}

export interface MountedAssemblySession {
  readonly transport: CadSessionTransport;
  readonly sessionId: string;
}

/**
 * Pair a session door with a session id.
 * Both are required before the virtual tree may ask the host for a page.
 */
export function resolveMountedAssemblySession(
  sessionTransport: CadSessionTransport | null | undefined,
  assemblySessionId: string | null | undefined,
): MountedAssemblySession | null {
  if (sessionTransport == null) return null;
  if (typeof assemblySessionId !== "string" || assemblySessionId.length === 0) return null;
  return { transport: sessionTransport, sessionId: assemblySessionId };
}

/** Shown until a host session is available for the assembly tab. */
export function assemblyTreeEmptyMessage(): string {
  return "加载 .aicad 或 .cadpack 会话后，可以在这里分页浏览大型装配。树的数据留在主机上，这里只显示当前这一页。";
}

function selectionStatus(graph: CanonicalGraph, nodeId: string, recordedRefs: readonly string[] | undefined): string {
  const node = graph.nodes[nodeId];
  const label = node?.name || nodeId;
  const refs = recordedRefs ?? (node?.state.status === "failed_with_rollback" ? node.state.failed_topo_refs : []);
  if (refs.length === 0) return `已选择 ${label}`;
  return `已选择 ${label}，标红 ${refs.join("、")}`;
}

const MESH_ASSET_FORMATS = new Set(["stl", "3mf", "obj", "gltf"]);
const OPEN_SHELL_NON_WATERTIGHT_HINT =
  "该网格非水密开放壳，无法直接作为闭合实体参与布尔，可作为参考网格预览";

function isMeshAssetFormat(format: string): boolean {
  return MESH_ASSET_FORMATS.has(format);
}

function buildOpenShellPreviewExpression(assetPath: string): string {
  return `import::mesh(path = "${assetPath}", allow_open_shell = true)`;
}

function buildOpenShellPreviewSource(assetPath: string): string {
  return `part ImportedAsset() -> Solid {\n    return ${buildOpenShellPreviewExpression(assetPath)}\n}\n`;
}

function ExternalAssetCard({ active, backend, onInsert }: {
  readonly active: boolean;
  readonly backend: WorkbenchBackend;
  readonly onInsert: (asset: WorkspaceAsset, options?: { readonly previewOpenShell?: boolean }) => void;
}) {
  const [hostAssets, setHostAssets] = useState<readonly WorkspaceAsset[]>([]);
  const [localAssets, setLocalAssets] = useState<readonly WorkspaceAsset[]>([]);
  const assets = [...hostAssets, ...localAssets.filter((asset) => !hostAssets.some((host) => host.path === asset.path))];
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState("");
  const [noticeTraceId, setNoticeTraceId] = useState<string | undefined>();
  const [inspection, setInspection] = useState<{
    readonly path: string;
    readonly result: AssetInspectionSummary;
  } | null>(null);
  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    setNotice("");
    setNoticeTraceId(undefined);
    try { await action(); } catch (err) {
      setNoticeTraceId(traceIdFrom(err));
      setNotice(err instanceof Error ? err.message : String(err));
    }
    finally { setBusy(false); }
  };
  const refresh = async () => {
    setHostAssets(await backend.listAssets());
  };
  const upload = async (file: File) => {
    if (!file.size || file.size > 32 * 1024 * 1024) throw new Error("请选择不超过 32 MiB 的非空文件");
    if (!/^[A-Za-z0-9][A-Za-z0-9_-]{0,95}\.(?:step|stp|brep|iges|igs|stl|3mf|obj|gltf|glb)(?![\s\S])/i.test(file.name)) {
      throw new Error("文件名须使用英文字母、数字、下划线或连字符，格式为 STEP、BREP、IGES、STL、3MF、OBJ 或 GLB / 内嵌 glTF");
    }
    const bytes = await file.arrayBuffer();
    const stepFile = /\.(?:step|stp)$/i.test(file.name);
    let uploaded: WorkspaceAsset;
    try {
      uploaded = await backend.uploadAsset(file.name, bytes);
    } catch (err) {
      if (backend.kind !== "http" || !stepFile || !(err instanceof WorkbenchUnavailableError)) throw err;
      rememberBrowserStepFile(file.name, bytes);
      const asset = localStepAsset(file.name, bytes.byteLength);
      setLocalAssets((current) => [...current.filter((item) => item.path !== asset.path), asset]);
      setNotice("工作台服务未连接，STEP 已留在这个页面。插入后由浏览器在后台读取原始网格。");
      return;
    }
    if (backend.kind === "http" && stepFile) rememberBrowserStepFile(file.name, bytes);
    setHostAssets((current) => [...current.filter((item) => item.path !== uploaded.path), uploaded]);
    setNotice("上传完成，可插入并预览");
  };
  return (
    <details className="card model-browser" open={active} onToggle={(event) => {
      if (event.currentTarget.open && !busy) void run(refresh);
    }}>
      <summary className="card-title">外部 3D 资产</summary>
      <p>上传 STEP、BREP、IGES、STL、3MF、OBJ 或 GLB / 内嵌 glTF（最多 32 MiB）</p>
      <input type="file" aria-label="上传外部 3D 资产" accept=".step,.stp,.brep,.iges,.igs,.stl,.3mf,.obj,.gltf,.glb" disabled={busy} onChange={(event) => {
        const file = event.currentTarget.files?.[0];
        event.currentTarget.value = "";
        if (file) void run(() => upload(file));
      }} />
      <button type="button" className="pill-btn" disabled={busy} onClick={() => void run(refresh)}>刷新</button>
      {notice && <p role="status">{notice}</p>}
      {noticeTraceId && <TraceIdChip traceId={noticeTraceId} />}
      <ul className="model-items">
        {assets.map((asset) => {
          const inspectedThis = inspection?.path === asset.path ? inspection.result : null;
          const isNonWatertightMesh = isMeshAssetFormat(asset.format) && inspectedThis?.watertight === false;
          const previewExpression = asset.previewExpression
            ?? (isMeshAssetFormat(asset.format) ? buildOpenShellPreviewExpression(asset.path) : undefined);
          return (
            <li key={asset.path}>
              <div>{asset.name}</div>
              <code>{asset.expression}</code>
              {isNonWatertightMesh && (
                <>
                  <p role="note">{OPEN_SHELL_NON_WATERTIGHT_HINT}</p>
                  {previewExpression && (
                    <div>
                      <small>参考网格预览：</small>
                      <code>{previewExpression}</code>
                    </div>
                  )}
                </>
              )}
              <div>
                <button type="button" className="pill-btn" disabled={busy} onClick={() => void run(async () => {
                  setInspection(null);
                  const result = (await backend.inspectAsset(asset)) as AssetInspectionSummary;
                  setInspection({ path: asset.path, result });
                  if (isMeshAssetFormat(asset.format) && result?.watertight === false) {
                    setNotice(`${asset.name} · ${OPEN_SHELL_NON_WATERTIGHT_HINT}`);
                  } else {
                    setNotice(`${asset.name} · 资产检查`);
                  }
                })}>检查</button>
                <button type="button" className="pill-btn" disabled={busy} onClick={() => onInsert(asset)}>
                  {isNonWatertightMesh ? "插入并预览（实体）" : "插入并预览"}
                </button>
                {isNonWatertightMesh && (
                  <button
                    type="button"
                    className="pill-btn"
                    disabled={busy}
                    onClick={() => onInsert(asset, { previewOpenShell: true })}
                  >
                    作为参考网格预览
                  </button>
                )}
              </div>
            </li>
          );
        })}
      </ul>
      {inspection != null && <details open><summary>检查详情</summary><pre style={{ maxHeight: 180, overflow: "auto" }}>{JSON.stringify(inspection.result, null, 2)}</pre></details>}
    </details>
  );
}

const TOKEN_COLOR_CLASS: Readonly<Record<KclTokenCategory, string>> = {
  comment: "kcl-tok-comment",
  annotation: "kcl-tok-annotation",
  keyword: "kcl-tok-keyword",
  type: "kcl-tok-type",
  namespace: "kcl-tok-namespace",
  dimension: "kcl-tok-dimension",
  number: "kcl-tok-number",
  constant: "kcl-tok-constant",
  pipe: "kcl-tok-pipe",
  string: "kcl-tok-string",
  operator: "kcl-tok-operator",
  plain: "kcl-tok-plain",
};

function isLineInSpan(lineNumber: number, span: SourceSpan | null): boolean {
  if (!span) {
    return false;
  }
  return lineNumber >= span.start_line && lineNumber <= span.end_line;
}

function getLineColFromOffset(
  source: string,
  offset: number,
): { readonly line: number; readonly col: number } {
  let line = 1;
  let col = 1;
  const limit = Math.min(offset, source.length);
  for (let i = 0; i < limit; i += 1) {
    if (source[i] === "\n") {
      line += 1;
      col = 1;
    } else {
      col += 1;
    }
  }
  return { line, col };
}

export function LeftPanel({
  activeTab,
  onSelectTab,
  embedded = false,
  backend,
  aiGenMode,
  onSelectAiMode,
  sceneMesh,
  selectedTarget,
  onSelectTarget,
  contract,
  verificationReport,
  diagnosticsReport,
  traceId,
  kclSource,
  highlightedSpan,
  highlightLabel,
  onChangeSource,
  onSelectSpan,
  onAiGenerate,
  aiConfigStatus,
  aiGenerating = false,
  aiFeedback,
  aiProgressSteps = [],
  graphIr,
  graphCacheStatus,
  selectedGraphNodeId,
  onEmitGraphPatch,
  onSelectGraphNode,
  activeProjectPath,
  onInitRemoteProject,
  onSyncRemotePatch,
  sessionTransport,
  assemblySessionId,
  onSelectAssemblyInstance,
  onAssemblyDigestReady,
  assemblyNotice,
  displayState,
  selectedComponentId,
  onSelectComponent,
  onToggleEntityHidden,
  onToggleEntityTranslucent,
  onIsolateEntity,
  onShowAllEntities,
  onStartSketchOnDatum,
  onAiRepairNode,
}: LeftPanelProps) {
  const mountedFeatureTree = useContext(FeatureTreeMountContext);
  const mountedAssemblyTree = useContext(AssemblyTreeMountContext);
  const featureTree = resolveFeatureTreeMount({
    graphIr,
    graphCacheStatus,
    selectedGraphNodeId,
    onEmitGraphPatch,
    onSelectGraphNode,
    activeProjectPath,
    onInitRemoteProject,
    onSyncRemotePatch,
  }, mountedFeatureTree, kclSource, INITIAL_DEMO_KCL);
  const assemblyTree = resolveAssemblyTreeMount({
    sessionTransport,
    assemblySessionId,
    onSelectAssemblyInstance,
    onAssemblyDigestReady,
    assemblyNotice,
  }, mountedAssemblyTree);
  const [initialUiPrefs] = useState(loadWorkbenchUiPrefs);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const codeViewRef = useRef<HTMLDivElement | null>(null);
  const [editMode, setEditMode] = useState<"highlighted" | "raw">(initialUiPrefs.kclEditMode);
  const [severityFilter, setSeverityFilter] = useState<
    "all" | CompilerDiagnostic["severity"]
  >(initialUiPrefs.diagnosticFilter);
  const [aiPrompt, setAiPrompt] = useState<string>(
    "设计一个 96x68x36mm 壁厚 2.5mm 的铝合金抽壳控制盒，带镜像安装耳和 M4x0.7 螺纹孔",
  );
  const [aiConfig, setAiConfig] = useState<StoredAiConfig>(loadAiConfig);
  const {
    provider: aiProvider,
    model: aiModel,
    baseUrl: aiBaseUrl,
    apiKey: aiApiKey,
    enableVlm: aiEnableVlm,
    fewShotK: aiFewShotK,
    material: aiMaterial,
    method: aiMethod,
  } = aiConfig;
  const updateAiConfig = (patch: Partial<StoredAiConfig>) => {
    setAiConfig((prev) => {
      const next = { ...prev, ...patch };
      saveAiConfig(next);
      return next;
    });
  };
  const [cursorPos, setCursorPos] = useState<{ readonly line: number; readonly col: number }>({
    line: 6,
    col: 5,
  });
  const [showLspDrawer, setShowLspDrawer] = useState<boolean>(initialUiPrefs.showLspDrawer);
  const [edgesExpanded, setEdgesExpanded] = useState(false);
  const [aicadDir, setAicadDir] = useState(
    () => `${featureTree.graphIr?.graph_id ?? "project"}.aicad`,
  );
  const [aicadName, setAicadName] = useState(() => featureTree.graphIr?.graph_id ?? "");
  const [lastGraphPatch, setLastGraphPatch] = useState<GraphPatch | null>(null);
  const [aicadBusy, setAicadBusy] = useState(false);
  const [modelSubTab, setModelSubTab] = useState<"browser" | "graph">(initialUiPrefs.modelSubTab);
  const [checksSubTab, setChecksSubTab] = useState<"diagnostics" | "contract">(initialUiPrefs.checksSubTab);

  useEffect(() => {
    updateWorkbenchUiPrefs({
      modelSubTab,
      checksSubTab,
      diagnosticFilter: severityFilter,
      kclEditMode: editMode,
      showLspDrawer,
    });
  }, [modelSubTab, checksSubTab, severityFilter, editMode, showLspDrawer]);
  // The activity bar does not list 装配树 yet. Remember it when this panel draws its own tabs.
  const [pinAssemblyTab, setPinAssemblyTab] = useState(false);
  const shownTab: LeftWorkbenchTab = pinAssemblyTab ? "assembly" : activeTab;
  const mountedAssemblySession = resolveMountedAssemblySession(
    assemblyTree.sessionTransport,
    assemblyTree.assemblySessionId,
  );
  const [topologyPage, setTopologyPage] = useState<{ scene: SceneMeshPayload; faces: number; edges: number }>();
  const pageSize = 100;
  const facePage = topologyPage?.scene === sceneMesh ? topologyPage.faces : 0;
  const edgePage = topologyPage?.scene === sceneMesh ? topologyPage.edges : 0;
  const pageControls = (kind: "faces" | "edges", page: number) => {
    const count = Math.ceil(sceneMesh[kind].length / pageSize);
    if (count < 2) return null;
    const go = (step: number) => setTopologyPage({ scene: sceneMesh, faces: facePage, edges: edgePage, [kind]: page + step });
    return (
      <div className="model-page-controls">
        <button type="button" className="pill-btn" disabled={page === 0} onClick={() => go(-1)}>上一页</button>
        <span>{page + 1} / {count}</span>
        <button type="button" className="pill-btn" disabled={page + 1 >= count} onClick={() => go(1)}>下一页</button>
      </div>
    );
  };

  const lines = useMemo(() => kclSource.split(/\r?\n/), [kclSource]);

  const lspResult = useMemo(
    () => analyzeKclLspFallback(kclSource, cursorPos.line, cursorPos.col),
    [kclSource, cursorPos],
  );

  const filteredDiagnostics = useMemo(() => {
    if (severityFilter === "all") {
      return diagnosticsReport.diagnostics;
    }
    return diagnosticsReport.diagnostics.filter(
      (diag) => diag.severity === severityFilter,
    );
  }, [diagnosticsReport.diagnostics, severityFilter]);

  const handleInsertCompletion = (item: LspCompletionItem) => {
    const edit = applyKclCompletion(kclSource, cursorPos.line, cursorPos.col, item.insert_text);
    onChangeSource(edit.source);
    setEditMode("raw");
    requestAnimationFrame(() => {
      const el = textareaRef.current;
      const caret = edit.source.slice(0, edit.caret).replace(/\r\n/g, "\n").length;
      el?.focus();
      el?.setSelectionRange(caret, caret);
    });
  };

  const activeHover: LspHoverInfo | null = lspResult.hover;

  useEffect(() => {
    if (shownTab !== "source" || !highlightedSpan) return;
    const view = codeViewRef.current;
    const line = view?.querySelector(".kcl-line-active");
    if (!view || !line) return;
    const lineBounds = line.getBoundingClientRect();
    const viewBounds = view.getBoundingClientRect();
    if (lineBounds.top < viewBounds.top || lineBounds.bottom > viewBounds.bottom) {
      view.scrollTop += lineBounds.top - viewBounds.top;
    }
  }, [shownTab, highlightedSpan, editMode]);

  useEffect(() => {
    setPinAssemblyTab(false);
  }, [activeTab]);

  const selectLeftTab = (tab: LeftWorkbenchTab) => {
    if (tab === "assembly") {
      setPinAssemblyTab(true);
      return;
    }
    setPinAssemblyTab(false);
    onSelectTab(tab);
  };

  const openSourceTab = () => {
    setPinAssemblyTab(false);
    onSelectTab("source");
    focusWorkbenchTab("left", "source");
  };

  const runAicadAction = (action: () => void | Promise<void>) => {
    setAicadBusy(true);
    void Promise.resolve()
      .then(action)
      .catch(() => {})
      .finally(() => setAicadBusy(false));
  };

  const emitGraphPatch = (patch: GraphPatch) => {
    setLastGraphPatch(patch);
    featureTree.onEmitGraphPatch?.(patch);
    if (featureTree.activeProjectPath && featureTree.onSyncRemotePatch) {
      void featureTree.onSyncRemotePatch(patch);
    }
  };

  const showAicadActions = Boolean(featureTree.onInitRemoteProject || featureTree.onSyncRemotePatch);
  const projectName = aicadName.trim() || featureTree.graphIr?.graph_id || "";
  const graphNodeCount = featureTree.graphIr ? Object.keys(featureTree.graphIr.nodes).length : 0;
  const graphFailedCount = featureTree.graphIr
    ? Object.values(featureTree.graphIr.nodes).filter((node) => node.state.status === "failed_with_rollback").length
    : 0;
  const activeModelSubTab: "browser" | "graph" = featureTree.graphIr ? modelSubTab : "browser";

  const modelSubTabs = useMemo(
    () =>
      featureTree.graphIr
        ? ([
            {
              id: "browser" as const,
              label: "模型浏览器",
              badge: sceneMesh.faces.length + sceneMesh.edges.length,
              isError: false,
            },
            {
              id: "graph" as const,
              label: "特征图 Graph IR",
              badge: graphFailedCount > 0 ? `${graphFailedCount} 异常` : graphNodeCount,
              isError: graphFailedCount > 0,
            },
          ] as const)
        : ([
            {
              id: "browser" as const,
              label: "模型浏览器",
              badge: sceneMesh.faces.length + sceneMesh.edges.length,
              isError: false,
            },
          ] as const),
    [featureTree.graphIr, graphFailedCount, graphNodeCount, sceneMesh.edges.length, sceneMesh.faces.length],
  );

  const checksSubTabs = useMemo(
    () =>
      [
        {
          id: "diagnostics" as const,
          label: "诊断列表",
          badge: diagnosticsReport.diagnostics.length,
          isError: diagnosticsReport.diagnostics.some((d) => d.severity === "error"),
        },
        {
          id: "contract" as const,
          label: "设计契约",
          badge: contract?.interfaces.length ?? 0,
          isError: false,
        },
      ] as const,
    [contract, diagnosticsReport.diagnostics],
  );

  return (
    <div className={embedded ? "panel-embedded" : "panel panel-left workbench-side"} aria-label="项目导航">
      {!embedded && (
        <WorkbenchTabs prefix="left" label="项目导航" tabs={LEFT_TABS} active={shownTab} onSelect={selectLeftTab} className="workbench-side-tabs" />
      )}
      <WorkbenchTabPanel prefix="left" id="model" active={shownTab === "model"} className="workbench-side-tabpanel">
        {modelSubTabs.length > 1 && (
          <WorkbenchTabs
            prefix="left-model-sub"
            label="模型与特征图视图"
            tabs={modelSubTabs}
            active={activeModelSubTab}
            onSelect={setModelSubTab}
            className="panel-subtabs"
            tabClassName={(_tab, active) => `panel-subtab${active ? " is-active" : ""}`}
            renderTab={(tab) => (
              <>
                <span>{tab.label}</span>
                {tab.badge !== undefined && tab.badge !== 0 && (
                  <span className={`panel-subtab-badge${tab.isError ? " is-error" : ""}`}>
                    {tab.badge}
                  </span>
                )}
              </>
            )}
          />
        )}

        <WorkbenchTabPanel
          prefix="left-model-sub"
          id="browser"
          active={activeModelSubTab === "browser"}
          className="panel-subtabpanel"
        >
          <section className="card model-browser">
            <div className="card-header">
              <h2 className="card-title">模型浏览器</h2>
              <span className="mini-badge">
                {sceneMesh.faces.length}F / {sceneMesh.edges.length}E
              </span>
            </div>
            <div className="model-browser-subhead">
              <p className="model-name">{sceneMesh.part_name}</p>
              {displayState && hasActiveDisplayOverrides(displayState) && onShowAllEntities && (
                <button
                  type="button"
                  className="mini-badge is-warn display-reset-btn"
                  title="显示全部已隐藏或孤立的对象"
                  onClick={onShowAllEntities}
                >
                  <CadIcon name="eye" size={11} />
                  <span>
                    {displayState.isolatedIds
                      ? "退出孤立"
                      : `显示全部 (${countHiddenEntities(displayState) || countTranslucentEntities(displayState)})`}
                  </span>
                </button>
              )}
            </div>
            {sceneMesh.components && sceneMesh.components.length > 0 && (
              <details open>
                <summary>组件 ({sceneMesh.components.length})</summary>
                {shownTab === "model" && activeModelSubTab === "browser" && (
                  <ul className="model-items display-pane-list">
                    {sceneMesh.components.map((comp) => {
                      const override = displayState?.componentOverrides[comp.component_id];
                      const isHidden = Boolean(override?.hidden);
                      const isTranslucent = (override?.opacity ?? 1) < 0.99;
                      const isIsolated = Boolean(displayState?.isolatedIds?.includes(comp.component_id));
                      const isSelectedComp = selectedComponentId === comp.component_id;
                      return (
                        <li
                          key={comp.component_id}
                          className={`display-pane-row${isHidden ? " is-hidden" : ""}${isIsolated ? " is-isolated" : ""}`}
                        >
                          <button
                            type="button"
                            className="display-pane-name"
                            aria-pressed={isSelectedComp}
                            onClick={() => onSelectComponent?.(isSelectedComp ? null : comp.component_id)}
                          >
                            {comp.instance_name || comp.part_name || comp.component_id}
                          </button>
                          {onToggleEntityHidden && onToggleEntityTranslucent && onIsolateEntity && (
                            <span className="display-pane-actions" role="group" aria-label={`${comp.component_id} 显示控制`}>
                              <button
                                type="button"
                                className={`dp-icon-btn${isHidden ? " is-active" : ""}`}
                                title={isHidden ? "显示组件" : "隐藏组件"}
                                aria-label={isHidden ? `显示 ${comp.component_id}` : `隐藏 ${comp.component_id}`}
                                aria-pressed={isHidden}
                                onClick={() => onToggleEntityHidden("component", comp.component_id)}
                              >
                                <CadIcon name={isHidden ? "eyeOff" : "eye"} size={12} />
                              </button>
                              <button
                                type="button"
                                className={`dp-icon-btn${isTranslucent ? " is-active" : ""}`}
                                title={isTranslucent ? "恢复不透明" : "设为半透明"}
                                aria-label={`切换 ${comp.component_id} 半透明`}
                                aria-pressed={isTranslucent}
                                onClick={() => onToggleEntityTranslucent("component", comp.component_id)}
                              >
                                <CadIcon name="translucent" size={12} />
                              </button>
                              <button
                                type="button"
                                className={`dp-icon-btn${isIsolated ? " is-active" : ""}`}
                                title={isIsolated ? "退出孤立" : "孤立此组件"}
                                aria-label={`孤立 ${comp.component_id}`}
                                aria-pressed={isIsolated}
                                onClick={() => onIsolateEntity(comp.component_id)}
                              >
                                <CadIcon name="isolate" size={12} />
                              </button>
                            </span>
                          )}
                        </li>
                      );
                    })}
                  </ul>
                )}
              </details>
            )}
            <details open>
              <summary>面 ({sceneMesh.faces.length})</summary>
              {shownTab === "model" && activeModelSubTab === "browser" && <ul className="model-items display-pane-list">
                {sceneMesh.faces.slice(facePage * pageSize, (facePage + 1) * pageSize).map((face) => {
                  const override = displayState?.faceOverrides[face.face_id];
                  const isHidden = Boolean(override?.hidden);
                  const isTranslucent = (override?.opacity ?? 1) < 0.99;
                  const isIsolated = Boolean(displayState?.isolatedIds?.includes(face.face_id));
                  return (
                    <li
                      key={face.face_id}
                      className={`display-pane-row${isHidden ? " is-hidden" : ""}${isIsolated ? " is-isolated" : ""}`}
                    >
                      <button
                        type="button"
                        className="display-pane-name"
                        aria-pressed={selectedTarget?.kind === "face" && selectedTarget.face.face_id === face.face_id}
                        onClick={() => onSelectTarget({ kind: "face", face })}
                      >
                        {face.face_id}
                      </button>
                      {onToggleEntityHidden && onToggleEntityTranslucent && onIsolateEntity && (
                        <span className="display-pane-actions" role="group" aria-label={`${face.face_id} 显示控制`}>
                          <button
                            type="button"
                            className={`dp-icon-btn${isHidden ? " is-active" : ""}`}
                            title={isHidden ? "显示面" : "隐藏面"}
                            aria-label={isHidden ? `显示 ${face.face_id}` : `隐藏 ${face.face_id}`}
                            aria-pressed={isHidden}
                            onClick={() => onToggleEntityHidden("face", face.face_id)}
                          >
                            <CadIcon name={isHidden ? "eyeOff" : "eye"} size={12} />
                          </button>
                          <button
                            type="button"
                            className={`dp-icon-btn${isTranslucent ? " is-active" : ""}`}
                            title={isTranslucent ? "恢复不透明" : "设为半透明"}
                            aria-label={`切换 ${face.face_id} 半透明`}
                            aria-pressed={isTranslucent}
                            onClick={() => onToggleEntityTranslucent("face", face.face_id)}
                          >
                            <CadIcon name="translucent" size={12} />
                          </button>
                          <button
                            type="button"
                            className={`dp-icon-btn${isIsolated ? " is-active" : ""}`}
                            title={isIsolated ? "退出孤立" : "孤立此面"}
                            aria-label={`孤立 ${face.face_id}`}
                            aria-pressed={isIsolated}
                            onClick={() => onIsolateEntity(face.face_id)}
                          >
                            <CadIcon name="isolate" size={12} />
                          </button>
                        </span>
                      )}
                    </li>
                  );
                })}
              </ul>}
              {pageControls("faces", facePage)}
            </details>
            <details onToggle={(event) => setEdgesExpanded(event.currentTarget.open)}>
              <summary>边 ({sceneMesh.edges.length})</summary>
              {shownTab === "model" && activeModelSubTab === "browser" && edgesExpanded && <ul className="model-items">
                {sceneMesh.edges.slice(edgePage * pageSize, (edgePage + 1) * pageSize).map((edge) => (
                  <li key={edge.edge_id}>
                    <button
                      type="button"
                      aria-pressed={selectedTarget?.kind === "edge" && selectedTarget.edge.edge_id === edge.edge_id}
                      onClick={() => onSelectTarget({ kind: "edge", edge })}
                    >
                      {edge.edge_id}
                    </button>
                  </li>
                ))}
              </ul>}
              {pageControls("edges", edgePage)}
            </details>
          </section>
        </WorkbenchTabPanel>

        {featureTree.graphIr ? (
          <WorkbenchTabPanel
            prefix="left-model-sub"
            id="graph"
            active={activeModelSubTab === "graph"}
            className="panel-subtabpanel"
          >
            <section className="card">
              <div className="card-header">
                <h2 className="card-title">特征图 Graph IR</h2>
                <span className={`mini-badge${graphFailedCount > 0 ? " badge-err" : " is-ok"}`}>
                  {graphFailedCount > 0 ? `${graphFailedCount} 节点回退` : `${graphNodeCount} 节点正常`}
                </span>
              </div>
              {featureTree.patchNotice ? <p role="status">{featureTree.patchNotice}</p> : null}
              {featureTree.patchTraceId ? <TraceIdChip traceId={featureTree.patchTraceId} /> : null}
              {featureTree.selectedGraphNodeId ? (
                <p role="status">
                  {selectionStatus(
                    featureTree.graphIr,
                    featureTree.selectedGraphNodeId,
                    featureTree.highlightedFailedTopoRefs ?? featureTree.highlightedTopoRefs,
                  )}
                </p>
              ) : null}
              {showAicadActions ? (
                <details className="aicad-sync-details">
                  <summary>工作区 .aicad 工程同步</summary>
                  <div className="aicad-project-actions" role="group" aria-label="工作区 aicad 工程" aria-busy={aicadBusy} style={{ display: "grid", gap: 8, margin: "8px 0" }}>
                    <label className="ai-config-field">
                      <span>工程目录</span>
                      <input
                        className="text-input"
                        aria-label="aicad 工程目录"
                        value={aicadDir}
                        disabled={aicadBusy}
                        onChange={(event) => setAicadDir(event.target.value)}
                      />
                    </label>
                    <label className="ai-config-field">
                      <span>工程名称</span>
                      <input
                        className="text-input"
                        aria-label="aicad 工程名称"
                        value={aicadName}
                        disabled={aicadBusy}
                        placeholder={featureTree.graphIr.graph_id}
                        onChange={(event) => setAicadName(event.target.value)}
                      />
                    </label>
                    {featureTree.activeProjectPath ? (
                      <p role="status">当前工程 <code>{featureTree.activeProjectPath}</code></p>
                    ) : (
                      <p className="muted-text">未连接工作区 .aicad。服务离线时特征图仍保存在本地。</p>
                    )}
                    <div className="pill-group">
                      <button
                        type="button"
                        className="pill-btn"
                        disabled={aicadBusy || !featureTree.onInitRemoteProject || !aicadDir.trim() || !projectName}
                        onClick={() => {
                          const init = featureTree.onInitRemoteProject;
                          if (!init) return;
                          runAicadAction(() => init(aicadDir.trim(), projectName));
                        }}
                      >
                        初始化 .aicad
                      </button>
                      <button
                        type="button"
                        className="pill-btn"
                        disabled={aicadBusy || !featureTree.onSyncRemotePatch || !lastGraphPatch}
                        title={lastGraphPatch ? "把最近一次特征图补丁写入 .aicad" : "先修改特征图，再同步补丁"}
                        onClick={() => {
                          const sync = featureTree.onSyncRemotePatch;
                          if (!sync || !lastGraphPatch) return;
                          runAicadAction(() => sync(lastGraphPatch));
                        }}
                      >
                        同步补丁
                      </button>
                    </div>
                  </div>
                </details>
              ) : null}
              <FaultTolerantFeatureTree
                graph={featureTree.graphIr}
                cacheStatus={featureTree.graphCacheStatus}
                selectedNodeId={featureTree.selectedGraphNodeId ?? null}
                onEmitPatch={emitGraphPatch}
                onSelectNode={(nodeId, failedTopoRefs) => {
                  featureTree.onSelectGraphNode?.(nodeId, failedTopoRefs);
                }}
                onRenameNode={featureTree.onRenameNode}
                onSetNodeSuppressed={featureTree.onSetNodeSuppressed}
                onUpsertParameter={featureTree.onUpsertParameter}
                onStartSketchOnDatum={onStartSketchOnDatum}
                onAiRepairNode={onAiRepairNode}
                showInlineInspector={false}
              />
              <ExplainModelPanel
                graph={featureTree.graphIr}
                nodeId={featureTree.selectedGraphNodeId ?? null}
                contract={contract}
              />
            </section>
          </WorkbenchTabPanel>
        ) : null}
      </WorkbenchTabPanel>

      <WorkbenchTabPanel prefix="left" id="assembly" active={shownTab === "assembly"} className="workbench-side-tabpanel">
        {shownTab === "assembly" ? (
          <div className="panel-stack">
            {/* Local scene Assembly BOM unified into Left Assembly Navigator (no duplicate right tab) */}
            {(sceneMesh.components?.length ?? diagnosticsReport.components?.length ?? 0) > 0 && (
              <section className="card" aria-label="装配体 BOM 清单">
                <div className="card-header">
                  <h2 className="card-title">
                    装配体 BOM ({(sceneMesh.components ?? diagnosticsReport.components ?? []).length})
                  </h2>
                  {selectedComponentId && onSelectComponent && (
                    <button type="button" className="pill-btn" onClick={() => onSelectComponent(null)}>
                      全部显示
                    </button>
                  )}
                </div>
                <ul className="model-items display-pane-list">
                  {(sceneMesh.components ?? diagnosticsReport.components ?? []).map((comp) => {
                    const override = displayState?.componentOverrides[comp.component_id];
                    const isHidden = Boolean(override?.hidden);
                    const isTranslucent = (override?.opacity ?? 1) < 0.99;
                    const isIsolated = Boolean(displayState?.isolatedIds?.includes(comp.component_id));
                    const isSelected = selectedComponentId === comp.component_id;
                    return (
                      <li
                        key={comp.component_id}
                        className={`display-pane-row${isHidden ? " is-hidden" : ""}${isIsolated ? " is-isolated" : ""}`}
                      >
                        <button
                          type="button"
                          className="display-pane-name"
                          aria-pressed={isSelected}
                          onClick={() => onSelectComponent?.(isSelected ? null : comp.component_id)}
                        >
                          {comp.instance_name || comp.part_name || comp.component_id}
                        </button>
                        {onToggleEntityHidden && onToggleEntityTranslucent && onIsolateEntity && (
                          <span className="display-pane-actions" role="group" aria-label={`${comp.component_id} 显示控制`}>
                            <button
                              type="button"
                              className={`dp-icon-btn${isHidden ? " is-active" : ""}`}
                              title={isHidden ? "显示组件" : "隐藏组件"}
                              onClick={() => onToggleEntityHidden("component", comp.component_id)}
                            >
                              <CadIcon name={isHidden ? "eyeOff" : "eye"} size={12} />
                            </button>
                            <button
                              type="button"
                              className={`dp-icon-btn${isTranslucent ? " is-active" : ""}`}
                              title={isTranslucent ? "恢复不透明" : "半透明"}
                              onClick={() => onToggleEntityTranslucent("component", comp.component_id)}
                            >
                              <CadIcon name="translucent" size={12} />
                            </button>
                            <button
                              type="button"
                              className={`dp-icon-btn${isIsolated ? " is-active" : ""}`}
                              title={isIsolated ? "退出孤立" : "孤立此组件"}
                              onClick={() => onIsolateEntity(comp.component_id)}
                            >
                              <CadIcon name="isolate" size={12} />
                            </button>
                          </span>
                        )}
                      </li>
                    );
                  })}
                </ul>
              </section>
            )}

            {mountedAssemblySession ? (
              <>
                {assemblyTree.assemblyNotice ? <p role="status">{assemblyTree.assemblyNotice}</p> : null}
                {assemblyTree.assemblyTraceId ? <TraceIdChip traceId={assemblyTree.assemblyTraceId} /> : null}
                <VirtualAssemblyTreePanel
                  transport={mountedAssemblySession.transport}
                  sessionId={mountedAssemblySession.sessionId}
                  onSelectInstance={assemblyTree.onSelectAssemblyInstance}
                  onDigestReady={assemblyTree.onAssemblyDigestReady}
                />
              </>
            ) : (
              <section className="card" aria-label="装配树">
                <h2 className="card-title">大型装配会话树</h2>
                <p className="muted-text">{assemblyTreeEmptyMessage()}</p>
              </section>
            )}
          </div>
        ) : null}
      </WorkbenchTabPanel>

      <WorkbenchTabPanel prefix="left" id="assets" active={shownTab === "assets"} className="workbench-side-tabpanel">
        <ExternalAssetCard active={shownTab === "assets"} backend={backend} onInsert={(asset, options) => {
          const nextSource = options?.previewOpenShell
            ? (asset.previewSource ?? buildOpenShellPreviewSource(asset.path))
            : asset.source;
          if (kclSource.trim() && kclSource !== nextSource && !window.confirm("用导入模型替换当前编辑器源码？")) return;
          onChangeSource(nextSource);
          openSourceTab();
        }} />
      </WorkbenchTabPanel>

      <WorkbenchTabPanel prefix="left" id="ai" active={shownTab === "ai"} className="workbench-side-tabpanel">
      {onAiGenerate && (
        <section className="card ai-gen-card">
          <h2 className="card-title ai-card-summary">
            <span>AI 生成</span>
            {aiConfigStatus && (
              <span
                className={`badge ${
                  aiConfigStatus.default_provider !== "template" &&
                  (aiConfigStatus.has_openai_key || aiConfigStatus.has_gemini_key)
                    ? "badge-ok"
                    : "badge-neutral"
                }`}
                title={`默认后端: ${aiConfigStatus.default_provider}`}
              >
                {aiConfigStatus.default_provider !== "template" &&
                (aiConfigStatus.has_openai_key || aiConfigStatus.has_gemini_key)
                  ? `LLM 就绪 (${aiConfigStatus.default_provider})`
                  : "离线模板"}
              </span>
            )}
          </h2>

          <div className="ai-mode-bar">
            <div className="pill-group" role="group" aria-label="AI 生成模式">
              <button
                type="button"
                className={`pill-btn ${aiGenMode === "generate" ? "active" : ""}`}
                aria-pressed={aiGenMode === "generate"}
                onClick={() => onSelectAiMode("generate")}
              >
                新建零件 (generate)
              </button>
              <button
                type="button"
                className={`pill-btn ${aiGenMode === "edit" ? "active" : ""}`}
                aria-pressed={aiGenMode === "edit"}
                onClick={() => onSelectAiMode("edit")}
              >
                修改当前 KCL (edit)
              </button>
            </div>
          </div>

          <textarea
            className="ai-prompt-input"
            rows={2}
            value={aiPrompt}
            onChange={(e) => setAiPrompt(e.target.value)}
            placeholder={
              aiGenMode === "edit"
                ? "描述要对当前模型做的修改，例如：把4个孔改为M5沉头孔，壁厚加到4mm"
                : "输入自然语言机械需求（支持抽壳盒、扫掠管道、支架、装配体）..."
            }
            aria-label="AI 自然语言设计需求"
          />

          <details className="ai-config-details">
            <summary>模型与接口配置</summary>
            <div className="ai-config-grid">
              <label className="ai-config-field">
                <span>Provider</span>
                <select
                  className="select-input"
                  value={aiProvider}
                  onChange={(e) => updateAiConfig({ provider: e.target.value as AiProvider })}
                  aria-label="AI Provider"
                >
                  <option value="auto">自动 (auto)</option>
                  <option value="template">离线模板 (template)</option>
                  <option value="openai">OpenAI 兼容 (openai)</option>
                  <option value="gemini">Gemini (gemini)</option>
                </select>
              </label>

              <label className="ai-config-field">
                <span>Model（可选）</span>
                <input
                  type="text"
                  className="text-input"
                  value={aiModel}
                  onChange={(e) => updateAiConfig({ model: e.target.value })}
                  placeholder={
                    aiProvider === "gemini"
                      ? aiConfigStatus?.gemini_model || "gemini-2.5-flash"
                      : aiConfigStatus?.openai_model || "gpt-4o-mini / 自定义模型"
                  }
                  aria-label="AI Model"
                />
              </label>

              <label className="ai-config-field">
                <span>Base URL（可选）</span>
                <input
                  type="url"
                  className="text-input"
                  value={aiBaseUrl}
                  onChange={(e) => updateAiConfig({ baseUrl: e.target.value })}
                  placeholder={
                    aiConfigStatus?.openai_base_url || "https://api.openai.com/v1"
                  }
                  aria-label="AI Base URL"
                />
              </label>

              <label className="ai-config-field">
                <span>API Key（本地记忆）</span>
                <input
                  type="password"
                  className="text-input"
                  autoComplete="off"
                  value={aiApiKey}
                  onChange={(e) => updateAiConfig({ apiKey: e.target.value })}
                  placeholder="留空则使用服务端环境变量"
                  aria-label="AI API Key"
                />
              </label>

              <label className="ai-config-field">
                <span>Few-shot 参考样本数</span>
                <select
                  className="select-input"
                  value={aiFewShotK}
                  onChange={(e) => updateAiConfig({ fewShotK: Number(e.target.value) })}
                  aria-label="Few-shot 参考样本数"
                >
                  <option value={0}>0（不使用）</option>
                  <option value={1}>1</option>
                  <option value={2}>2（默认）</option>
                  <option value={3}>3</option>
                </select>
              </label>

              <div className="ai-config-field">
                <label className="checkbox-inline ai-config-checkbox">
                  <input
                    type="checkbox"
                    checked={aiEnableVlm}
                    onChange={(e) => updateAiConfig({ enableVlm: e.target.checked })}
                    aria-label="启用 VLM 四视图审查"
                  />
                  <span>启用 VLM 四视图审查</span>
                </label>
                <span className="ai-config-hint">仅在真实 LLM 模式下生效</span>
              </div>
            </div>
          </details>

          <div className="ai-controls-row">
            <select
              className="select-input"
              value={aiMaterial}
              onChange={(e) => updateAiConfig({ material: e.target.value })}
              aria-label="AI 生成材料"
            >
              <option value="Al-6061-T6">Al-6061-T6</option>
              <option value="Ti-6Al-4V">Ti-6Al-4V</option>
              <option value="SUS304">SUS304</option>
            </select>
            <select
              className="select-input"
              value={aiMethod}
              onChange={(e) => updateAiConfig({ method: e.target.value })}
              aria-label="AI 生成工艺"
            >
              <option value="CNC_3AXIS_MILLING">CNC 3轴铣削</option>
              <option value="CNC_5AXIS_MILLING">CNC 5轴铣削</option>
              <option value="ADDITIVE_SLM">金属 3D 打印</option>
            </select>
            <button
              type="button"
              className="btn-primary"
              disabled={aiGenerating || !aiPrompt.trim()}
              onClick={() => {
                const llm: AiLlmConfig = {
                  provider: aiProvider,
                  ...(aiModel.trim() ? { model: aiModel.trim() } : {}),
                  ...(aiBaseUrl.trim() ? { base_url: aiBaseUrl.trim() } : {}),
                  ...(aiApiKey.trim() ? { api_key: aiApiKey.trim() } : {}),
                  enable_vlm: aiEnableVlm,
                  few_shot_k: aiFewShotK,
                };
                void onAiGenerate(aiPrompt, aiMaterial, aiMethod, {
                  mode: aiGenMode,
                  llm,
                });
              }}
            >
              {aiGenerating
                ? aiGenMode === "edit"
                  ? "正在增量修改..."
                  : "正在生成..."
                : aiGenMode === "edit"
                  ? "应用增量修改"
                  : "生成 KCL 源码"}
            </button>
          </div>

          {aiGenerating && aiProgressSteps.length === 0 && (
            <div className="ai-feedback ai-feedback-loading" role="status">
              正在调用 AI 引擎处理，请稍候...
            </div>
          )}

          {aiProgressSteps.length > 0 && (
            <ol className="ai-progress-timeline" aria-label="AI 执行进度" role="status">
              {aiProgressSteps.map((step, idx) => {
                const isLatest = idx === aiProgressSteps.length - 1;
                const statusClass =
                  step.passed === false
                    ? "is-error"
                    : step.passed === true || step.stage === "completed"
                      ? "is-ok"
                      : aiGenerating && isLatest
                        ? "is-active"
                        : "is-done";
                const icon =
                  step.passed === false
                    ? "✕"
                    : step.passed === true || step.stage === "completed"
                      ? "✓"
                      : aiGenerating && isLatest
                        ? "●"
                        : "•";
                return (
                  <li key={`${step.stage}-${idx}`} className={`ai-progress-item ${statusClass}`}>
                    <span className="ai-progress-icon" aria-hidden="true">
                      {icon}
                    </span>
                    {typeof step.iteration === "number" && (
                      <span className="ai-progress-iter">
                        R{step.iteration}
                        {typeof step.max_iters === "number" ? `/${step.max_iters}` : ""}
                      </span>
                    )}
                    <span className="ai-progress-msg">{step.message}</span>
                  </li>
                );
              })}
            </ol>
          )}

          {!aiGenerating && aiFeedback && (
            <div
              className={`ai-feedback ${
                aiFeedback.status === "error"
                  ? "ai-feedback-error"
                  : "ai-feedback-success"
              }`}
              role={aiFeedback.status === "error" ? "alert" : "status"}
            >
              <div className="ai-feedback-msg">{aiFeedback.message}</div>
              {aiFeedback.detail && (
                <div className="ai-feedback-detail">{aiFeedback.detail}</div>
              )}
              {(aiFeedback.vlmEnabled !== undefined ||
                (aiFeedback.retrievedExamples &&
                  aiFeedback.retrievedExamples.length > 0) ||
                (aiFeedback.contractRepairAttempts !== undefined &&
                  aiFeedback.contractRepairAttempts > 0)) && (
                <div className="ai-feedback-meta">
                  {aiFeedback.vlmEnabled !== undefined && (
                    <span className="ai-feedback-tag">
                      VLM 审查: {aiFeedback.vlmEnabled ? "已启用" : "未启用"}
                    </span>
                  )}
                  {aiFeedback.contractRepairAttempts !== undefined &&
                    aiFeedback.contractRepairAttempts > 0 && (
                      <span className="ai-feedback-tag">
                        契约修复: {aiFeedback.contractRepairAttempts} 次
                      </span>
                    )}
                  {aiFeedback.retrievedExamples &&
                    aiFeedback.retrievedExamples.length > 0 && (
                      <span className="ai-feedback-examples">
                        参考样本: {aiFeedback.retrievedExamples.join(", ")}
                      </span>
                    )}
                </div>
              )}
            </div>
          )}
        </section>
      )}
      </WorkbenchTabPanel>

      <WorkbenchTabPanel prefix="left" id="checks" active={shownTab === "checks"} className="workbench-side-tabpanel">
        <WorkbenchTabs
          prefix="left-checks-sub"
          label="诊断与契约视图"
          tabs={checksSubTabs}
          active={checksSubTab}
          onSelect={setChecksSubTab}
          className="panel-subtabs"
          tabClassName={(_tab, active) => `panel-subtab${active ? " is-active" : ""}`}
          renderTab={(tab) => (
            <>
              <span>{tab.label}</span>
              {tab.badge !== undefined && tab.badge !== 0 && (
                <span className={`panel-subtab-badge${tab.isError ? " is-error" : ""}`}>
                  {tab.badge}
                </span>
              )}
            </>
          )}
        />

        <WorkbenchTabPanel
          prefix="left-checks-sub"
          id="diagnostics"
          active={checksSubTab === "diagnostics"}
          className="panel-subtabpanel"
        >
          <section className="card diagnostics-card">
            <div className="card-header">
              <h2 className="card-title">诊断 ({diagnosticsReport.diagnostics.length})</h2>
              {traceId && diagnosticsReport.status === "error" ? <TraceIdChip traceId={traceId} /> : null}
              <div className="pill-group" role="group" aria-label="诊断级别筛选">
                {(["all", "error", "warning", "info"] as const).map((level) => (
                  <button
                    key={level}
                    type="button"
                    className={`pill-btn ${severityFilter === level ? "active" : ""}`}
                    onClick={() => setSeverityFilter(level)}
                  >
                    {level.toUpperCase()}
                  </button>
                ))}
              </div>
            </div>
            <ul className="diag-list">
              {filteredDiagnostics.map((diag, idx) => (
                <li key={`${diag.code}-${idx}`}>
                  <button
                    type="button"
                    className={`diag-item diag-${diag.severity}`}
                    onClick={() => {
                      onSelectSpan(diag.span, `${diag.code} (L${diag.span.start_line})`);
                      openSourceTab();
                    }}
                  >
                    <div className="diag-item-top">
                      <span className={`severity-dot severity-${diag.severity}`} />
                      <span className="diag-code">{diag.code}</span>
                      <span className="diag-loc">
                        L{diag.span.start_line}:{diag.span.start_col}
                      </span>
                    </div>
                    <div className="diag-msg">{diag.message}</div>
                  </button>
                </li>
              ))}
            </ul>
          </section>
        </WorkbenchTabPanel>

        <WorkbenchTabPanel
          prefix="left-checks-sub"
          id="contract"
          active={checksSubTab === "contract"}
          className="panel-subtabpanel"
        >
          <section className="card contract-card">
            <h2 className="card-title">设计契约</h2>
            {contract ? (
              <>
                <div className="contract-summary">
                  {verificationReport.summary ?? contract.summary ?? contract.part_name}
                </div>
                <p className="workbench-panel-empty">验证状态 {verificationReport.overall_status}</p>
                <dl className="contract-grid">
                  <div>
                    <dt>零件</dt>
                    <dd>{contract.part_name}</dd>
                  </div>
                  <div>
                    <dt>材料</dt>
                    <dd>
                      {typeof contract.material === "string"
                        ? contract.material
                        : contract.material.name}
                    </dd>
                  </div>
                  <div>
                    <dt>制造工艺</dt>
                    <dd>{contract.manufacturing_method}</dd>
                  </div>
                  <div>
                    <dt>Keep-In 包络</dt>
                    <dd>
                      [{contract.keep_in_bbox_mm.min.join(", ")}] .. [
                      {contract.keep_in_bbox_mm.max.join(", ")}]{" "}
                      {typeof contract.units === "string"
                        ? contract.units
                        : contract.units.length}
                    </dd>
                  </div>
                  <div>
                    <dt>配合接口</dt>
                    <dd>
                      {contract.interfaces
                        .map((item) => item.tolerance ?? item.interface_id)
                        .join(" · ") || "无"}
                    </dd>
                  </div>
                </dl>
                <ValidationPanel
                  report={verificationReport}
                  onJump={(target) => {
                    featureTree.onJumpToViolation?.({ nodeId: target.nodeId, topoRef: target.topoRef });
                  }}
                />
              </>
            ) : (
              <>
                <div className="contract-summary">No design contract attached</div>
                <ValidationPanel
                  report={verificationReport}
                  onJump={(target) => {
                    featureTree.onJumpToViolation?.({ nodeId: target.nodeId, topoRef: target.topoRef });
                  }}
                />
              </>
            )}
          </section>
        </WorkbenchTabPanel>
      </WorkbenchTabPanel>

      <WorkbenchTabPanel prefix="left" id="source" active={shownTab === "source"} className="workbench-side-tabpanel">
      <section className="card editor-card">
        <h2 className="card-title" title={highlightLabel ?? undefined}>
          KCL 源码{highlightedSpan && ` · 第 ${highlightedSpan.start_line} 行`}
        </h2>
        <div className="card-header">
          <div className="pill-group">
            <button
              type="button"
              className={`pill-btn ${showLspDrawer ? "active" : ""}`}
              onClick={() => setShowLspDrawer((prev) => !prev)}
            >
              LSP 补全
            </button>
            <button
              type="button"
              className={`pill-btn ${editMode === "highlighted" ? "active" : ""}`}
              onClick={() => setEditMode("highlighted")}
            >
              高亮
            </button>
            <button
              type="button"
              className={`pill-btn ${editMode === "raw" ? "active" : ""}`}
              onClick={() => setEditMode("raw")}
            >
              直接编辑
            </button>
          </div>
        </div>

        {(showLspDrawer || activeHover) && (
          <div className="lsp-card">
            {activeHover && (
              <div className="lsp-hover-box">
                <div className="lsp-hover-title">
                  <strong>{activeHover.symbol}</strong>
                  <span className="badge badge-accent">{activeHover.dimension_type}</span>
                </div>
                <div className="lsp-hover-summary">{activeHover.summary}</div>
              </div>
            )}
            {showLspDrawer && (
              <div className="lsp-completion-row">
                {lspResult.completions.slice(0, 8).map((item) => (
                  <button
                    key={`${item.kind}-${item.label}`}
                    type="button"
                    className="topo-chip"
                    title={item.detail}
                    onClick={() => handleInsertCompletion(item)}
                  >
                    {item.label}
                  </button>
                ))}
              </div>
            )}
          </div>
        )}

        {editMode === "highlighted" ? (
          <div ref={codeViewRef} className="kcl-code-view" role="region" aria-label="CAD-DSL 语法高亮视图">
            {lines.map((line, index) => {
              const lineNum = index + 1;
              const isHighlighted = isLineInSpan(lineNum, highlightedSpan);
              const tokens = tokenizeKclLine(line);
              return (
                <div
                  key={lineNum}
                  className={`kcl-line ${isHighlighted ? "kcl-line-active" : ""}`}
                  onClick={() => {
                    const firstIdent = /[a-zA-Z_][\w:]*/.exec(line);
                    setCursorPos({
                      line: lineNum,
                      col: firstIdent ? (firstIdent.index ?? 0) + 1 : 1,
                    });
                  }}
                >
                  <span className="kcl-gutter">{lineNum}</span>
                  <code className="kcl-line-code">
                    {tokens.length === 0 ? "\u00A0" : null}
                    {tokens.map((tok, tokIdx) => (
                      <span key={tokIdx} className={TOKEN_COLOR_CLASS[tok.category]}>
                        {tok.text}
                      </span>
                    ))}
                  </code>
                </div>
              );
            })}
          </div>
        ) : (
          <textarea
            ref={textareaRef}
            className="kcl-raw-textarea"
            value={kclSource}
            onChange={(e) => {
              const nextVal = e.target.value;
              onChangeSource(nextVal);
              setCursorPos(
                getLineColFromOffset(nextVal, e.target.selectionStart ?? 0),
              );
            }}
            onClick={(e) => {
              const target = e.currentTarget;
              setCursorPos(
                getLineColFromOffset(target.value, target.selectionStart ?? 0),
              );
            }}
            spellCheck={false}
            aria-label="CAD-DSL 源码编辑器"
          />
        )}
      </section>
      </WorkbenchTabPanel>
    </div>
  );
}
