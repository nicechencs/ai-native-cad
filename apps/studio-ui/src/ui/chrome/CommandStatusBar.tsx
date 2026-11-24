import { useEffect, useRef, useState } from "react";
import { COMMAND_LINE_SUGGESTIONS } from "../../core/commands/commandLineParser.ts";
import { loadWorkbenchUiPrefs, updateWorkbenchUiPrefs } from "../../core/layout/workbenchPrefsStore.ts";
import {
  getNavigationPresetMeta,
  NAVIGATION_PRESETS,
  type NavigationPresetId,
} from "../../core/viewport/navigationPreset.ts";
import { CadIcon } from "../icons/CadIcons.tsx";

export interface CommandStatusBarProps {
  readonly onSubmitCommandLine: (raw: string) => void;
  readonly compiling: boolean;
  readonly statusOk: boolean;
  readonly errorCount: number;
  readonly watertight: boolean;
  readonly openEdgeCount?: number;
  readonly faceCount: number;
  readonly edgeCount: number;
  readonly compileTimeMs?: number;
  readonly activeModeLabel: string | null;
  readonly onExitActiveMode?: () => void;
  readonly unreadNotifications: number;
  readonly onToggleNotifications: () => void;
  readonly units: string;
  readonly navigationPreset: NavigationPresetId;
  readonly onChangeNavigationPreset: (preset: NavigationPresetId) => void;
  readonly onRegenerate?: () => void;
}

export function CommandStatusBar({
  onSubmitCommandLine,
  compiling,
  statusOk,
  errorCount,
  watertight,
  openEdgeCount,
  faceCount,
  edgeCount,
  compileTimeMs,
  activeModeLabel,
  onExitActiveMode,
  unreadNotifications,
  onToggleNotifications,
  units,
  navigationPreset,
  onChangeNavigationPreset,
  onRegenerate,
}: CommandStatusBarProps) {
  const [value, setValue] = useState("");
  const [history, setHistory] = useState<readonly string[]>(() => loadWorkbenchUiPrefs().commandHistory);
  const [historyCursor, setHistoryCursor] = useState(-1);
  const [showHints, setShowHints] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    const handleGlobalKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "/" || event.ctrlKey || event.metaKey || event.altKey) return;
      const target = event.target;
      if (
        target instanceof HTMLElement &&
        (target.tagName === "INPUT" ||
          target.tagName === "TEXTAREA" ||
          target.tagName === "SELECT" ||
          target.isContentEditable)
      ) {
        return;
      }
      event.preventDefault();
      inputRef.current?.focus();
    };
    window.addEventListener("keydown", handleGlobalKeyDown);
    return () => window.removeEventListener("keydown", handleGlobalKeyDown);
  }, []);

  const handleSubmit = (event: React.FormEvent) => {
    event.preventDefault();
    const trimmed = value.trim();
    if (!trimmed) return;
    onSubmitCommandLine(trimmed);
    setHistory((prev) => {
      const next = [trimmed, ...prev.filter((item) => item !== trimmed)].slice(0, 30);
      updateWorkbenchUiPrefs({ commandHistory: next });
      return next;
    });
    setHistoryCursor(-1);
    setValue("");
    setShowHints(false);
  };

  const navMeta = getNavigationPresetMeta(navigationPreset);

  return (
    <footer className="studio-statusbar" aria-label="命令与状态栏">
      <form className="cmdline-form" onSubmit={handleSubmit}>
        <span className="cmdline-prompt" aria-hidden="true">
          <CadIcon name="terminal" size={13} />
        </span>
        <input
          ref={inputRef}
          type="text"
          className="cmdline-input"
          aria-label="交互式命令行"
          placeholder="输入命令 (ext 20 / hole 8 / fillet 2 / shell 2.5 / sketch / iso / /ai 加4个M4孔)…"
          value={value}
          onFocus={() => setShowHints(true)}
          onBlur={() => window.setTimeout(() => setShowHints(false), 160)}
          onChange={(event) => {
            setValue(event.target.value);
            setHistoryCursor(-1);
          }}
          onKeyDown={(event) => {
            if (event.key === "ArrowUp" && history.length > 0) {
              event.preventDefault();
              const nextCursor = Math.min(history.length - 1, historyCursor + 1);
              setHistoryCursor(nextCursor);
              setValue(history[nextCursor]);
            } else if (event.key === "ArrowDown") {
              event.preventDefault();
              if (historyCursor <= 0) {
                setHistoryCursor(-1);
                setValue("");
              } else {
                const nextCursor = historyCursor - 1;
                setHistoryCursor(nextCursor);
                setValue(history[nextCursor]);
              }
            } else if (event.key === "Escape") {
              setValue("");
              setShowHints(false);
              inputRef.current?.blur();
            }
          }}
        />
        {showHints && !value && (
          <div className="cmdline-hints-popover" role="region" aria-label="常用命令提示">
            <div className="cmdline-hints-title">快捷命令语法（按 / 聚焦）</div>
            {COMMAND_LINE_SUGGESTIONS.slice(0, 8).map((item) => (
              <button
                key={item.syntax}
                type="button"
                className="cmdline-hint-row"
                onMouseDown={(event) => {
                  event.preventDefault();
                  setValue(item.syntax.split(" / ")[0]);
                  inputRef.current?.focus();
                }}
              >
                <code>{item.syntax}</code>
                <span>{item.description}</span>
              </button>
            ))}
          </div>
        )}
      </form>

      <div className="statusbar-right">
        {activeModeLabel && (
          <button
            type="button"
            className="status-mode-pill"
            onClick={onExitActiveMode}
            title="点击或按 Esc 退出当前模式"
          >
            <span>{activeModeLabel}</span>
            <CadIcon name="close" size={12} />
          </button>
        )}

        <button
          type="button"
          className={`status-pill status-pill-btn${compiling ? " is-compiling" : !statusOk ? " is-error" : " is-ok"}`}
          role="status"
          title="点击重新再生与验证 (Ctrl+G)"
          onClick={onRegenerate}
        >
          {compiling ? (
            <>
              <CadIcon name="spinner" size={12} />
              <span>再生中…</span>
            </>
          ) : !statusOk ? (
            <>
              <CadIcon name="error" size={12} />
              <span>{errorCount} 项错误</span>
            </>
          ) : (
            <>
              <CadIcon name="check" size={12} />
              <span>就绪{compileTimeMs !== undefined ? ` · ${compileTimeMs.toFixed(0)}ms` : ""}</span>
            </>
          )}
        </button>

        {statusOk && (
          <span
            className={`status-pill${watertight ? " is-ok-subtle" : " is-warn"}`}
            title={watertight ? "零开放边水密流形" : `存在 ${openEdgeCount ?? "未知"} 条开放边`}
          >
            {watertight ? "水密 ✔" : `开放边 ${openEdgeCount ?? "!"}`}
          </span>
        )}

        <span className="status-metric tabular-nums" title="当前拓扑统计">
          {faceCount} 面 · {edgeCount} 边
        </span>

        <span className="status-unit" title="当前工程长度单位">
          {units}
        </span>

        <label className="status-nav-select-wrap" title={`鼠标导航方案：${navMeta.description}`}>
          <CadIcon name="mouse" size={12} />
          <select
            className="status-nav-select"
            aria-label="鼠标导航预设"
            value={navigationPreset}
            onChange={(event) => onChangeNavigationPreset(event.target.value as NavigationPresetId)}
          >
            {NAVIGATION_PRESETS.map((preset) => (
              <option key={preset.id} value={preset.id}>
                {preset.shortLabel}
              </option>
            ))}
          </select>
        </label>

        <button
          type="button"
          className={`status-bell-btn${unreadNotifications > 0 ? " has-unread" : ""}`}
          onClick={onToggleNotifications}
          aria-label="打开通知与操作日志"
          title="通知中心与事件日志"
        >
          <CadIcon name="bell" size={14} />
          {unreadNotifications > 0 && (
            <span className="bell-badge">{unreadNotifications > 9 ? "9+" : unreadNotifications}</span>
          )}
        </button>
      </div>
    </footer>
  );
}
