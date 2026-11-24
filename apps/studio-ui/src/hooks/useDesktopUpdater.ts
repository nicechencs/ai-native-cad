import { useCallback, useEffect, useRef, useState } from "react";
import type { PushNotificationInput } from "../core/notifications/notificationStore.ts";
import type {
  AppUpdateCheckResult,
  AppUpdateInstallResult,
  AppUpdaterInfo,
  AppUpdaterProgressEvent,
  WorkbenchBackend,
} from "../sync/workbenchBackend.ts";

export type DesktopUpdaterStatus =
  | "unsupported"
  | "idle"
  | "checking"
  | "available"
  | "up_to_date"
  | "unconfigured"
  | "downloading"
  | "installed"
  | "error";

export interface DesktopUpdaterState {
  readonly supported: boolean;
  readonly configured: boolean;
  readonly status: DesktopUpdaterStatus;
  readonly currentVersion: string;
  readonly endpoint: string;
  readonly availableVersion: string | null;
  readonly releaseDate: string | null;
  readonly releaseNotes: string | null;
  readonly downloadUrl: string | null;
  readonly downloadedBytes: number;
  readonly totalBytes: number | null;
  readonly progressPercent: number | null;
  readonly message: string;
  readonly busy: boolean;
}

export interface DesktopUpdaterController {
  readonly state: DesktopUpdaterState;
  checkForUpdates(): Promise<AppUpdateCheckResult | null>;
  installUpdate(): Promise<AppUpdateInstallResult | null>;
}

export function createInitialDesktopUpdaterState(supported: boolean): DesktopUpdaterState {
  if (!supported) {
    return {
      supported: false,
      configured: false,
      status: "unsupported",
      currentVersion: "web",
      endpoint: "",
      availableVersion: null,
      releaseDate: null,
      releaseNotes: null,
      downloadUrl: null,
      downloadedBytes: 0,
      totalBytes: null,
      progressPercent: null,
      message: "当前运行在浏览器模式；请使用官方桌面安装包（NSIS / DMG / AppImage）启用签名自动更新。",
      busy: false,
    };
  }
  return {
    supported: true,
    configured: false,
    status: "idle",
    currentVersion: "",
    endpoint: "",
    availableVersion: null,
    releaseDate: null,
    releaseNotes: null,
    downloadUrl: null,
    downloadedBytes: 0,
    totalBytes: null,
    progressPercent: null,
    message: "点击“检查更新”查询最新官方签名版本。",
    busy: false,
  };
}

export function applyUpdaterInfo(
  state: DesktopUpdaterState,
  info: AppUpdaterInfo,
): DesktopUpdaterState {
  return {
    ...state,
    supported: true,
    configured: info.configured,
    currentVersion: info.currentVersion,
    endpoint: info.endpoint,
    status: info.configured ? (state.status === "unconfigured" ? "idle" : state.status) : "unconfigured",
    message: info.message,
  };
}

export function applyUpdateCheckResult(
  state: DesktopUpdaterState,
  result: AppUpdateCheckResult,
): DesktopUpdaterState {
  return {
    ...state,
    supported: true,
    configured: result.configured,
    currentVersion: result.currentVersion || state.currentVersion,
    status: result.status,
    availableVersion: result.availableVersion ?? null,
    releaseDate: result.releaseDate ?? null,
    releaseNotes: result.releaseNotes ?? null,
    downloadUrl: result.downloadUrl ?? null,
    downloadedBytes: 0,
    totalBytes: null,
    progressPercent: null,
    message: result.message,
    busy: false,
  };
}

export function applyUpdaterProgress(
  state: DesktopUpdaterState,
  event: AppUpdaterProgressEvent,
): DesktopUpdaterState {
  if (event.event === "started") {
    return {
      ...state,
      status: "downloading",
      busy: true,
      downloadedBytes: 0,
      totalBytes: event.totalBytes ?? null,
      progressPercent: 0,
      message: "正在下载签名更新包并校验完整性…",
    };
  }
  if (event.event === "finished") {
    return {
      ...state,
      status: "downloading",
      busy: true,
      progressPercent: 100,
      message: "下载完成，正在验证签名并安装更新…",
    };
  }
  const total = event.totalBytes && event.totalBytes > 0 ? event.totalBytes : state.totalBytes;
  const pct = total && total > 0 ? Math.min(100, Math.round((event.downloadedBytes / total) * 100)) : null;
  return {
    ...state,
    status: "downloading",
    busy: true,
    downloadedBytes: event.downloadedBytes,
    totalBytes: total,
    progressPercent: pct,
    message: pct !== null
      ? `正在下载更新包（${pct}% · ${formatByteProgress(event.downloadedBytes, total)}）…`
      : `正在下载更新包（${formatByteProgress(event.downloadedBytes, null)}）…`,
  };
}

export function applyUpdateInstallResult(
  state: DesktopUpdaterState,
  result: AppUpdateInstallResult,
): DesktopUpdaterState {
  const status: DesktopUpdaterStatus =
    result.status === "installed"
      ? "installed"
      : result.status === "unconfigured"
        ? "unconfigured"
        : "up_to_date";
  return {
    ...state,
    status,
    busy: false,
    progressPercent: result.status === "installed" ? 100 : null,
    message: result.message,
  };
}

export function applyUpdaterError(
  state: DesktopUpdaterState,
  error: unknown,
): DesktopUpdaterState {
  const detail = error instanceof Error && error.message.trim()
    ? error.message.trim()
    : typeof error === "string" && error.trim()
      ? error.trim()
      : "检查或安装更新时发生未知错误。";
  return {
    ...state,
    status: "error",
    busy: false,
    message: detail,
  };
}

export function formatByteProgress(downloadedBytes: number, totalBytes: number | null): string {
  const formatUnit = (bytes: number): string => {
    if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
    if (bytes >= 1024) return `${(bytes / 1024).toFixed(0)} KB`;
    return `${bytes} B`;
  };
  if (totalBytes && totalBytes > 0) {
    return `${formatUnit(downloadedBytes)} / ${formatUnit(totalBytes)}`;
  }
  return formatUnit(downloadedBytes);
}

export function useDesktopUpdater(
  backend: WorkbenchBackend,
  notify?: (input: PushNotificationInput) => void,
): DesktopUpdaterController {
  const supported = Boolean(
    backend.kind === "tauri" &&
    backend.getAppUpdaterInfo &&
    backend.checkAppUpdate &&
    backend.installAppUpdate,
  );
  const [state, setState] = useState<DesktopUpdaterState>(() =>
    createInitialDesktopUpdaterState(supported),
  );
  const busyRef = useRef(false);
  const startupCheckedRef = useRef(false);

  useEffect(() => {
    setState(createInitialDesktopUpdaterState(supported));
    startupCheckedRef.current = false;
    if (!supported || !backend.getAppUpdaterInfo || !backend.checkAppUpdate) return;
    let cancelled = false;
    void (async () => {
      try {
        const info = await backend.getAppUpdaterInfo!();
        if (cancelled) return;
        setState((prev) => applyUpdaterInfo(prev, info));
        if (!info.configured || startupCheckedRef.current) return;
        startupCheckedRef.current = true;
        const result = await backend.checkAppUpdate!();
        if (cancelled) return;
        setState((prev) => applyUpdateCheckResult(prev, result));
        if (result.status === "available" && result.availableVersion) {
          notify?.({
            level: "info",
            title: `发现新版本 v${result.availableVersion}`,
            detail: "可前往「文件 › 键鼠与工作台偏好」查看更新说明并一键安装。",
            source: "更新",
          });
        }
      } catch {
        // Silent background startup check should never disrupt offline CAD modeling.
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [backend, notify, supported]);

  const checkForUpdates = useCallback(async (): Promise<AppUpdateCheckResult | null> => {
    if (!supported || !backend.checkAppUpdate || busyRef.current) return null;
    busyRef.current = true;
    setState((prev) => ({
      ...prev,
      status: "checking",
      busy: true,
      message: "正在检查最新签名版本…",
    }));
    try {
      const result = await backend.checkAppUpdate();
      setState((prev) => applyUpdateCheckResult(prev, result));
      if (result.status === "available" && result.availableVersion) {
        notify?.({
          level: "info",
          title: `发现新版本 v${result.availableVersion}`,
          detail: result.message,
          source: "更新",
        });
      }
      return result;
    } catch (error) {
      setState((prev) => applyUpdaterError(prev, error));
      notify?.({
        level: "warning",
        title: "检查更新失败",
        detail: error instanceof Error ? error.message : String(error),
        source: "更新",
      });
      return null;
    } finally {
      busyRef.current = false;
    }
  }, [backend, notify, supported]);

  const installUpdate = useCallback(async (): Promise<AppUpdateInstallResult | null> => {
    if (!supported || !backend.installAppUpdate || busyRef.current) return null;
    busyRef.current = true;
    setState((prev) => ({
      ...prev,
      status: "downloading",
      busy: true,
      downloadedBytes: 0,
      totalBytes: null,
      progressPercent: 0,
      message: "正在准备下载签名更新包…",
    }));
    try {
      const result = await backend.installAppUpdate(
        { restartAfterInstall: true },
        (event) => {
          setState((prev) => applyUpdaterProgress(prev, event));
        },
      );
      setState((prev) => applyUpdateInstallResult(prev, result));
      if (result.status === "installed") {
        notify?.({
          level: "success",
          title: `更新 v${result.installedVersion ?? ""} 已就绪`,
          detail: result.message,
          source: "更新",
        });
      }
      return result;
    } catch (error) {
      setState((prev) => applyUpdaterError(prev, error));
      notify?.({
        level: "error",
        title: "安装更新失败",
        detail: error instanceof Error ? error.message : String(error),
        source: "更新",
      });
      return null;
    } finally {
      busyRef.current = false;
    }
  }, [backend, notify, supported]);

  return {
    state,
    checkForUpdates,
    installUpdate,
  };
}
