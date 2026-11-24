import { useCallback, useMemo, useState } from "react";
import { AssemblyTreeMountProvider, FeatureTreeMountProvider } from "./components/LeftPanel.tsx";
import { WorkbenchLayout } from "./components/WorkbenchLayout.tsx";
import {
  createInitialNotificationState,
  pushNotification,
} from "./core/notifications/notificationStore.ts";
import { EngineeringSessionProvider, useEngineeringSession } from "./hooks/useEngineeringSession.ts";
import {
  applyAicadGraphPatch,
  emitAicadKcl,
  initAicadProject,
  ProjectClientError,
  type ProjectClientOptions,
} from "./sync/projectClient.ts";
import {
  CadSessionTransportError,
  createSessionTransport,
  type HlodLevel,
  type RoiInterfaceDigest,
  type VirtualTreeNode,
} from "./sync/cadSessionTransport.ts";
import { HlodChunkStreamer } from "./sync/hlodStreamer.ts";
import { createWorkbenchBackend, type WorkbenchBackend } from "./sync/workbenchBackend.ts";
import type { CacheStatus, GraphPatch, ParameterDef } from "./types/graphIr.ts";
import type { SceneMeshPayload } from "./types/scene.ts";
import { createProgressiveViewportController } from "./utils/hlodViewportBridge.ts";

/** Remote bridge succeeded, or the dev server was unreachable and the in-memory graph stays usable. */
export interface RemoteProjectActionResult {
  readonly mode: "remote" | "local_fallback";
  readonly accepted: boolean;
  readonly projectPath: string | null;
  readonly notice: string;
  readonly kcl: string | null;
  /** Present when the project bridge generated or echoed a trace id. */
  readonly traceId?: string;
}

/** Read a 16- or 32-character hex trace id from a project or session result. */
export function actionTraceId(value: unknown): string | undefined {
  if (!value || typeof value !== "object") return undefined;
  const id = (value as { traceId?: unknown }).traceId;
  if (typeof id !== "string") return undefined;
  const trimmed = id.trim().toLowerCase();
  return /^[0-9a-f]{16}$|^[0-9a-f]{32}$/.test(trimmed) ? trimmed : undefined;
}

/** Status 0 is the project client's "fetch never reached the dev server" signal. */
export function isOfflineProjectError(error: unknown): boolean {
  return error instanceof ProjectClientError && error.status === 0;
}

/**
 * Keep a path the dev server will accept on the next call.
 * Init and apply-patch often report an absolute location, which the bridge rejects.
 */
export function retainWorkspaceRelativePath(reported: string, fallback: string): string {
  const usable = reported.trim();
  const drive = /^[A-Za-z]:/.test(usable);
  const escapes = usable.split(/[\\/]/).includes("..");
  if (usable && !usable.includes("\0") && !usable.startsWith("/") && !usable.startsWith("\\") && !drive && !escapes) {
    return usable;
  }
  return fallback;
}

/**
 * Create a workspace `.aicad` project.
 * When the dev server is offline, keep the relative directory so later syncs can retry.
 */
export async function initRemoteProjectWithFallback(
  relativeDir: string,
  name: string,
  options?: ProjectClientOptions,
): Promise<RemoteProjectActionResult> {
  const relative = relativeDir.trim();
  const projectName = name.trim();
  if (!relative || !projectName) {
    return {
      mode: "local_fallback",
      accepted: false,
      projectPath: null,
      notice: "请填写工程目录和名称。",
      kcl: null,
    };
  }
  try {
    const created = await initAicadProject(relative, projectName, options);
    const projectPath = retainWorkspaceRelativePath(created.path, relative);
    const located = projectPath === created.path ? "" : "（" + created.path + "）";
    const traceId = actionTraceId(created);
    return {
      mode: "remote",
      accepted: true,
      projectPath,
      notice: "已初始化远程工程 " + created.project_name + located,
      kcl: null,
      ...(traceId ? { traceId } : {}),
    };
  } catch (error) {
    const traceId = actionTraceId(error);
    if (isOfflineProjectError(error)) {
      return {
        mode: "local_fallback",
        accepted: true,
        projectPath: relative,
        notice: "开发服务未连接，工程保留在本地，特征图仍可编辑。",
        kcl: null,
        ...(traceId ? { traceId } : {}),
      };
    }
    const message = error instanceof Error ? error.message : String(error);
    return {
      mode: "local_fallback",
      accepted: false,
      projectPath: null,
      notice: "远程初始化失败：" + message,
      kcl: null,
      ...(traceId ? { traceId } : {}),
    };
  }
}

/**
 * Write one graph patch to the `.aicad` project and export KCL.
 * A dead dev server leaves the patch in the local feature graph.
 */
export async function syncRemotePatchWithFallback(
  relativePath: string,
  patch: GraphPatch,
  options?: ProjectClientOptions,
): Promise<RemoteProjectActionResult> {
  const relative = relativePath.trim();
  if (!relative) {
    return {
      mode: "local_fallback",
      accepted: true,
      projectPath: null,
      notice: "尚未初始化 .aicad 工程，补丁只写在本地特征图。",
      kcl: null,
    };
  }
  let patched: Awaited<ReturnType<typeof applyAicadGraphPatch>>;
  try {
    patched = await applyAicadGraphPatch(relative, patch, options);
  } catch (error) {
    const traceId = actionTraceId(error);
    if (isOfflineProjectError(error)) {
      return {
        mode: "local_fallback",
        accepted: true,
        projectPath: relative,
        notice: "开发服务未连接，补丁只写在本地特征图。",
        kcl: null,
        ...(traceId ? { traceId } : {}),
      };
    }
    const message = error instanceof Error ? error.message : String(error);
    return {
      mode: "local_fallback",
      accepted: false,
      projectPath: null,
      notice: "远程同步失败：" + message,
      kcl: null,
      ...(traceId ? { traceId } : {}),
    };
  }

  const projectPath = retainWorkspaceRelativePath(patched.path, relative);
  try {
    const emitted = await emitAicadKcl(projectPath, options);
    const traceId = actionTraceId(emitted) ?? actionTraceId(patched);
    return {
      mode: "remote",
      accepted: true,
      projectPath,
      notice: "已同步补丁 " + patched.patch_id + "，已导出 KCL",
      kcl: emitted.kcl,
      ...(traceId ? { traceId } : {}),
    };
  } catch (error) {
    const offline = isOfflineProjectError(error);
    const message = error instanceof Error ? error.message : String(error);
    const traceId = actionTraceId(error) ?? actionTraceId(patched);
    return {
      mode: "remote",
      accepted: true,
      projectPath,
      notice: offline
        ? "已同步补丁 " + patched.patch_id + "，导出 KCL 时开发服务断开。"
        : "已同步补丁 " + patched.patch_id + "，KCL 尚未导出：" + message,
      kcl: null,
      ...(traceId ? { traceId } : {}),
    };
  }
}

const defaultBackend = createWorkbenchBackend();

/**
 * Choose the host session for the virtual assembly tree.
 * An `.aicad` or `.cadpack` path contributes its file name without the extension.
 * A connected workbench with no such file uses the demo session.
 */
export function resolveAssemblySessionId(
  activeProjectPath: string | null | undefined,
  connected = false,
): string | null {
  const raw = typeof activeProjectPath === "string" ? activeProjectPath.trim() : "";
  if (raw) {
    const base = raw.split(/[\\/]/).pop() ?? "";
    const stem = base.replace(/\.(?:aicad|cadpack)$/i, "");
    const usable = stem !== base
      && stem.length > 0
      && stem.length <= 256
      && stem !== "."
      && stem !== ".."
      && !stem.includes("..")
      && !/[\u0000-\u001f\u007f/\\?#]/.test(stem);
    if (usable) return stem;
  }
  if (connected) return "demo";
  return null;
}

export function formatAssemblyInstanceNotice(node: VirtualTreeNode): string {
  const name = node.name.trim();
  if (name) return "已选择装配实例 " + name;
  const part = node.part_number.trim();
  if (part) return "已选择装配实例 " + part;
  return "已选择装配实例 #" + String(node.instance_id);
}

export function chunkIdsForPrototype(prototypeId: number): Readonly<Record<HlodLevel, number>> {
  const base = prototypeId * 10;
  return { hlod0: base, hlod1: base + 1, hlod2: base + 2 };
}

export interface AssemblyDigestNotice {
  readonly title: string;
  readonly detail: string;
}

export function formatAssemblyDigestNotice(componentCount: number): AssemblyDigestNotice {
  const count = Number.isFinite(componentCount) && componentCount > 0
    ? Math.trunc(componentCount)
    : 0;
  return {
    title: "装配接口摘要已就绪",
    detail: String(count) + " 个组件",
  };
}

function noticeLine(title: string, detail?: string): string {
  return detail ? `${title}：${detail}` : title;
}

export function App({ backend = defaultBackend }: { readonly backend?: WorkbenchBackend }) {
  return (
    <EngineeringSessionProvider>
      <WorkbenchApp backend={backend} />
    </EngineeringSessionProvider>
  );
}

function WorkbenchApp({ backend }: { readonly backend: WorkbenchBackend }) {
  const {
    graphIr,
    revision,
    activeProjectPath,
    setActiveProjectPath,
    selectedNodeId,
    selectedTopoRefs,
    applySemanticCommands,
    dispatchSemanticCommand,
  } = useEngineeringSession();
  const graphCacheStatus: CacheStatus = revision === 0 ? "fresh_exact_match" : "graph_modified";
  // Toasts stay in WorkbenchLayout. This log is the feature-tree notice source.
  const [notifications, setNotifications] = useState(createInitialNotificationState);
  const latestFeatureNotice = notifications.logs.find((item) => item.source === "特征图");
  const patchNotice = latestFeatureNotice === undefined
    ? undefined
    : noticeLine(latestFeatureNotice.title, latestFeatureNotice.detail);
  const patchTraceId = latestFeatureNotice?.traceId;
  const latestAssemblyNotice = notifications.logs.find((item) => item.source === "装配树");
  const assemblyNotice = latestAssemblyNotice === undefined
    ? undefined
    : noticeLine(latestAssemblyNotice.title, latestAssemblyNotice.detail);
  const assemblyTraceId = latestAssemblyNotice?.traceId;

  const sessionTransport = useMemo(() => createSessionTransport(), []);
  const assemblySessionId = useMemo(
    () => resolveAssemblySessionId(activeProjectPath, true),
    [activeProjectPath],
  );

  const [, setAssemblyPreview] = useState<{ scene: SceneMeshPayload; level: HlodLevel } | null>(null);

  const logNotice = useCallback((
    level: "info" | "error",
    title: string,
    detail?: string,
    source = "特征图",
    traceId?: string,
  ) => {
    setNotifications((current) => pushNotification(current, {
      level,
      title,
      detail,
      source,
      showToast: false,
      ...(traceId ? { traceId } : {}),
    }).state);
  }, []);

  const hlodStreamer = useMemo(() => new HlodChunkStreamer({ transport: sessionTransport }), [sessionTransport]);
  const progressiveController = useMemo(() => createProgressiveViewportController({
    streamer: hlodStreamer,
    onSceneReady(scene, level) {
      setAssemblyPreview({ scene, level });
      if (level !== "hlod2") logNotice("info", "装配预览已细化", level.toUpperCase(), "装配树");
    },
    onError(error) {
      if (error instanceof CadSessionTransportError && (error.status === 0 || error.status === 404)) return;
      logNotice("error", "装配预览加载失败", error instanceof Error ? error.message : String(error), "装配树", actionTraceId(error));
    },
  }), [hlodStreamer, logNotice]);

  const onSelectAssemblyInstance = useCallback((node: VirtualTreeNode) => {
    logNotice("info", formatAssemblyInstanceNotice(node), undefined, "装配树");
    if (!assemblySessionId) return;
    void progressiveController.focusInstance({
      sessionId: assemblySessionId,
      node,
      chunkIdsByLevel: chunkIdsForPrototype(node.prototype_id),
      cameraDistanceMm: 120,
    }).catch(() => { /* The controller's onError handles the active request. */ });
  }, [assemblySessionId, logNotice, progressiveController]);

  const onAssemblyDigestReady = useCallback((digest: RoiInterfaceDigest) => {
    const notice = formatAssemblyDigestNotice(digest.component_count);
    logNotice("info", notice.title, notice.detail, "装配树");
  }, [logNotice]);

  const onEmitGraphPatch = useCallback((patch: GraphPatch) => {
    try {
      applySemanticCommands([{ op: "apply_graph_patch", patch }], patch.description);
      logNotice("info", patchNoticeTitle(patch));
    } catch (err) {
      logNotice("error", "特征图补丁未应用", err instanceof Error ? err.message : String(err), "特征图", actionTraceId(err));
    }
  }, [applySemanticCommands, logNotice]);

  const onSelectGraphNode = useCallback((nodeId: string, failedTopoRefs: readonly string[]) => {
    dispatchSemanticCommand({ op: "select", nodeId, topoRefs: failedTopoRefs, nodeIds: [nodeId] });
  }, [dispatchSemanticCommand]);

  const onRenameNode = useCallback((nodeId: string, name: string) => {
    try {
      dispatchSemanticCommand({ op: "rename_node", nodeId, name });
      logNotice("info", `已重命名 ${nodeId}`);
    } catch (err) {
      logNotice("error", "重命名失败", err instanceof Error ? err.message : String(err));
    }
  }, [dispatchSemanticCommand, logNotice]);

  const onSetNodeSuppressed = useCallback((nodeId: string, suppressed: boolean) => {
    try {
      dispatchSemanticCommand({ op: "set_node_suppressed", nodeId, suppressed });
      logNotice("info", suppressed ? `已抑制 ${nodeId}` : `已取消抑制 ${nodeId}，节点待重算`);
    } catch (err) {
      logNotice("error", "抑制状态未更新", err instanceof Error ? err.message : String(err));
    }
  }, [dispatchSemanticCommand, logNotice]);

  const onUpsertParameter = useCallback((value: ParameterDef) => {
    try {
      dispatchSemanticCommand({ op: "upsert_parameter", value });
      logNotice("info", `已更新参数 ${value.name}`);
    } catch (err) {
      logNotice("error", "参数未更新", err instanceof Error ? err.message : String(err));
    }
  }, [dispatchSemanticCommand, logNotice]);

  const onJumpToViolation = useCallback((target: { readonly nodeId: string | null; readonly topoRef: string | null }) => {
    dispatchSemanticCommand({
      op: "select",
      nodeId: target.nodeId,
      nodeIds: target.nodeId ? [target.nodeId] : [],
      topoRefs: target.topoRef ? [target.topoRef] : [],
    });
  }, [dispatchSemanticCommand]);

  const onInitRemoteProject = useCallback((relativeDir: string, name: string) => {
    return initRemoteProjectWithFallback(relativeDir, name).then((result) => {
      if (result.projectPath) setActiveProjectPath(result.projectPath);
      logNotice(result.accepted ? "info" : "error", result.notice, undefined, "特征图", result.traceId);
    });
  }, [logNotice]);

  const onSyncRemotePatch = useCallback((patch: GraphPatch) => {
    return syncRemotePatchWithFallback(activeProjectPath ?? "", patch).then((result) => {
      if (result.projectPath) setActiveProjectPath(result.projectPath);
      logNotice(result.accepted ? "info" : "error", result.notice, undefined, "特征图", result.traceId);
    });
  }, [activeProjectPath, logNotice]);

  const featureTree = useMemo(() => ({
    graphIr,
    graphCacheStatus,
    selectedGraphNodeId: selectedNodeId,
    highlightedTopoRefs: selectedTopoRefs,
    highlightedFailedTopoRefs: selectedTopoRefs,
    activeProjectPath,
    patchNotice,
    patchTraceId,
    onEmitGraphPatch,
    onSelectGraphNode,
    onRenameNode,
    onSetNodeSuppressed,
    onUpsertParameter,
    onJumpToViolation,
    onInitRemoteProject,
    onSyncRemotePatch,
  }), [
    activeProjectPath,
    graphCacheStatus,
    graphIr,
    onEmitGraphPatch,
    onInitRemoteProject,
    onJumpToViolation,
    onRenameNode,
    onSelectGraphNode,
    onSetNodeSuppressed,
    onSyncRemotePatch,
    onUpsertParameter,
    patchNotice,
    patchTraceId,
    selectedNodeId,
    selectedTopoRefs,
  ]);

  const assemblyTree = useMemo(() => ({
    sessionTransport,
    assemblySessionId,
    onSelectAssemblyInstance,
    onAssemblyDigestReady,
    assemblyNotice,
    assemblyTraceId,
  }), [
    assemblyNotice,
    assemblyTraceId,
    assemblySessionId,
    onAssemblyDigestReady,
    onSelectAssemblyInstance,
    sessionTransport,
  ]);

  return (
    <FeatureTreeMountProvider value={featureTree}>
      <AssemblyTreeMountProvider value={assemblyTree}>
        <WorkbenchLayout backend={backend} activeProjectPath={activeProjectPath} />
      </AssemblyTreeMountProvider>
    </FeatureTreeMountProvider>
  );
}

function patchNoticeTitle(patch: GraphPatch): string {
  if (patch.ops.length === 1) {
    const op = patch.ops[0];
    if (op?.op === "set_node_state") {
      return op.value.state.status === "suppressed"
        ? `已抑制 ${op.value.id}`
        : `已取消抑制 ${op.value.id}，节点待重算`;
    }
    if (op?.op === "upsert_parameter") {
      return `已更新参数 ${op.value.name}`;
    }
  }
  return patch.description;
}

export default App;
