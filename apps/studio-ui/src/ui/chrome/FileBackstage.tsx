import { useEffect, useState } from "react";
import {
  isCommandEnabled,
  type CommandContext,
  type CommandDefinition,
} from "../../core/commands/commandRegistry.ts";
import {
  NAVIGATION_PRESETS,
  type NavigationPresetId,
} from "../../core/viewport/navigationPreset.ts";
import { MATERIAL_PRESETS } from "../../data/demoWorkbenchState.ts";
import type { DesktopUpdaterController } from "../../hooks/useDesktopUpdater.ts";
import type { MaterialPreset } from "../../types/scene.ts";
import { CadIcon, type CadIconName } from "../icons/CadIcons.tsx";

type BackstageSection = "document" | "export" | "prepare" | "preferences";

export interface FileBackstageProps {
  readonly open: boolean;
  readonly onClose: () => void;
  readonly documentName: string;
  readonly partName: string;
  readonly activeBranch: string;
  readonly activeProjectPath: string | null;
  readonly commandMap: ReadonlyMap<string, CommandDefinition>;
  readonly context: CommandContext;
  readonly onExecute: (commandId: string) => void;
  readonly selectedMaterial: MaterialPreset;
  readonly onSelectMaterial: (material: MaterialPreset) => void;
  readonly navigationPreset: NavigationPresetId;
  readonly onChangeNavigationPreset: (preset: NavigationPresetId) => void;
  readonly units: string;
  readonly updater?: DesktopUpdaterController;
}

/**
 * Creo / NX style File Backstage panel.
 * Replaces putting file/export/demo reset commands inside a normal modeling ribbon tab.
 */
export function FileBackstage({
  open,
  onClose,
  documentName,
  partName,
  activeBranch,
  activeProjectPath,
  commandMap,
  context,
  onExecute,
  selectedMaterial,
  onSelectMaterial,
  navigationPreset,
  onChangeNavigationPreset,
  units,
  updater,
}: FileBackstageProps) {
  const [section, setSection] = useState<BackstageSection>("document");

  useEffect(() => {
    if (!open) return;
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        onClose();
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [open, onClose]);

  if (!open) return null;

  const runCmd = (commandId: string) => {
    onClose();
    onExecute(commandId);
  };

  const renderActionCard = (commandId: string, title: string, desc: string, icon: CadIconName, primary = false) => {
    const cmd = commandMap.get(commandId);
    const enabled = cmd ? isCommandEnabled(cmd, context) : true;
    const reason = cmd && !enabled ? cmd.disabledReason?.(context) : undefined;
    return (
      <button
        key={commandId}
        type="button"
        className={`backstage-action-card${primary ? " is-primary" : ""}`}
        disabled={!enabled}
        title={reason ?? desc}
        onClick={() => runCmd(commandId)}
      >
        <span className="backstage-action-icon">
          <CadIcon name={icon} size={18} />
        </span>
        <span className="backstage-action-body">
          <strong>{title}</strong>
          <span>{reason ?? desc}</span>
        </span>
        {cmd?.shortcut ? <kbd className="shortcut-kbd">{cmd.shortcut}</kbd> : null}
      </button>
    );
  };

  return (
    <div
      className="backstage-backdrop"
      role="dialog"
      aria-modal="true"
      aria-label="文件后台菜单"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div className="file-backstage">
        <nav className="backstage-nav" aria-label="文件后台分类">
          <button
            type="button"
            className="backstage-back-btn"
            onClick={onClose}
            title="返回 3D 工作台 (Esc)"
          >
            <CadIcon name="close" size={14} />
            <span>返回工作台</span>
          </button>

          <div className="backstage-doc-summary">
            <strong>{partName || documentName}</strong>
            <span>{activeProjectPath ?? documentName}</span>
            <span className="mini-badge">分支 {activeBranch}</span>
          </div>

          <button
            type="button"
            className={`backstage-nav-item${section === "document" ? " is-active" : ""}`}
            onClick={() => setSection("document")}
          >
            <CadIcon name="fileMenu" size={15} />
            <span>文档与互操作</span>
          </button>
          <button
            type="button"
            className={`backstage-nav-item${section === "export" ? " is-active" : ""}`}
            onClick={() => setSection("export")}
          >
            <CadIcon name="export" size={15} />
            <span>工程交付导出</span>
          </button>
          <button
            type="button"
            className={`backstage-nav-item${section === "prepare" ? " is-active" : ""}`}
            onClick={() => setSection("prepare")}
          >
            <CadIcon name="properties" size={15} />
            <span>模型准备与材料</span>
          </button>
          <button
            type="button"
            className={`backstage-nav-item${section === "preferences" ? " is-active" : ""}`}
            onClick={() => setSection("preferences")}
          >
            <CadIcon name="mouse" size={15} />
            <span>键鼠与工作台偏好</span>
            {updater?.state.status === "available" && (
              <span className="mini-badge is-ok" style={{ marginLeft: "auto" }}>有更新</span>
            )}
          </button>
        </nav>

        <div className="backstage-content">
          {section === "document" && (
            <section className="backstage-section">
              <h3>工程文档与代码视图</h3>
              <p className="backstage-desc">
                统一管理 .aicad 工程、.kcl 双向声明式源码以及外部 3D B-Rep / 网格资产导入。
              </p>
              <div className="backstage-grid">
                {renderActionCard("file.loadSource", "载入 KCL 源码…", "从本地打开 .kcl 文件并投影至当前工程", "code")}
                {renderActionCard("file.downloadSource", "保存 / 下载 KCL 源码", "导出当前 1:1 声明式 KCL 源码视图", "export", true)}
                {renderActionCard("file.openAicad", "打开 .aicad 工程…", "打开本地 .aicad 工程并恢复完整多体与建模历史", "code")}
                {renderActionCard("file.saveAicad", "保存 .aicad 工程", "原地保存当前工程图并保留 oplog 与建模分支历史", "export")}
                {renderActionCard("file.importAsset", "导入外部 3D 资产…", "支持 STEP / IGES / BREP / STL / 3MF / OBJ / GLB", "assetImport")}
                {renderActionCard("model.regenerate", "重建与 L0–L4 验证", "重新评估特征图并刷新五层三值工程门禁", "refresh")}
                {renderActionCard("file.resetDemo", "载入默认样板（法兰轴承座）", "将当前工程重置为默认参数化机械样板（可撤销）", "cube")}
              </div>
            </section>
          )}

          {section === "export" && (
            <section className="backstage-section">
              <h3>工程交付与交换格式导出</h3>
              <p className="backstage-desc">
                基于 OCCT 7.9+ 单一权威几何内核生成精确 B-Rep 交付物，支持 STEP AP242（含 PMI / XCAF）、精确 HLR 工程图与增材 3MF。
              </p>
              <div className="backstage-grid">
                {renderActionCard("export.step", "导出 STEP AP242 (含 PMI)", "工业精确 B-Rep 实体与装配 XCAF 交换格式", "export", true)}
                {renderActionCard("export.drawing", "导出 2D 关联工程图 SVG", "带隐藏线消除 (HLR) 与尺寸标注的矢量工程图", "drawing2d")}
                {renderActionCard("export.dxf", "导出 DXF 工程轮廓", "用于激光切割 / CNC 轮廓加工的 2D DXF", "drawing2d")}
                {renderActionCard("export.3mf", "导出 3MF 增材制造模型", "带精确拓扑与单位信息的 3D 打印封装格式", "cube")}
                {renderActionCard("export.mesh", "导出 Scene-Mesh JSON", "包含面/边/血缘标签的轻量化场景网格快照", "code")}
                {renderActionCard("panel.openExport", "打开右侧导出与日志面板", "查看实时导出日志与属性保留边界说明", "sidebarRight")}
              </div>
            </section>
          )}

          {section === "prepare" && (
            <section className="backstage-section">
              <h3>模型准备（对标 Creo File › Prepare）</h3>
              <p className="backstage-desc">
                配置工程单位制、单一事实源（schemas/materials.json）材料牌号与设计契约审查。
              </p>
              <div className="backstage-prepare-card">
                <div className="backstage-prepare-row">
                  <span>工程长度单位</span>
                  <strong className="tabular-nums">{units}（公制标准）</strong>
                </div>
                <label className="backstage-prepare-field">
                  <span>工程材料牌号（单一事实源 SSOT）</span>
                  <select
                    className="select-input"
                    value={selectedMaterial.id}
                    onChange={(event) => {
                      const found = MATERIAL_PRESETS.find((item) => item.id === event.target.value);
                      if (found) onSelectMaterial(found);
                    }}
                  >
                    {MATERIAL_PRESETS.map((mat) => (
                      <option key={mat.id} value={mat.id}>
                        {mat.id} — {mat.name}（{mat.density_g_cm3} g/cm³ · 屈服 {mat.yield_strength_mpa} MPa）
                      </option>
                    ))}
                  </select>
                </label>
                <div className="backstage-grid">
                  {renderActionCard("panel.openChecks", "审查设计契约与 L0–L4 诊断", "核查 Keep-In 包络、载荷、公差接口与三值门禁", "shieldCheck")}
                  {renderActionCard("panel.openProperties", "查看质量特性与参数表", "打开右侧属性检查器查看体积、质量与参数公式", "properties")}
                </div>
              </div>
            </section>
          )}

          {section === "preferences" && (
            <section className="backstage-section">
              <h3>鼠标导航习惯与工作台偏好</h3>
              <p className="backstage-desc">
                默认遵循 Siemens NX / PTC Creo 工业 CAD 肌肉记忆（中键旋转 + 左键点选/框选 + 中键确认），也可随时切换至 SolidWorks 或 Web 触控板模式。
              </p>
              <div className="backstage-preset-list">
                {NAVIGATION_PRESETS.map((preset) => {
                  const active = navigationPreset === preset.id;
                  return (
                    <button
                      key={preset.id}
                      type="button"
                      className={`backstage-preset-card${active ? " is-active" : ""}`}
                      aria-pressed={active}
                      onClick={() => onChangeNavigationPreset(preset.id)}
                    >
                      <div className="backstage-preset-head">
                        <strong>{preset.label}</strong>
                        {active && <span className="mini-badge is-ok">当前启用</span>}
                      </div>
                      <p>{preset.description}</p>
                      <div className="backstage-preset-hints">
                        <span>旋转: {preset.orbitHint}</span>
                        <span>平移: {preset.panHint}</span>
                        <span>缩放: {preset.zoomHint}</span>
                        <span>选择: {preset.selectHint}</span>
                      </div>
                    </button>
                  );
                })}
              </div>
              <div className="backstage-grid" style={{ marginTop: 12 }}>
                {renderActionCard("theme.toggle", "切换深色工业 / 浅色蓝图主题", "在深色工程暗房与浅色蓝图模式间即时切换", "sun")}
                {renderActionCard("help.shortcuts", "打开快捷键与命令行速查", "查看全部工业键位、径向手势与 / 快捷命令语法", "keyboard")}
              </div>

              {updater && (
                <div className="backstage-updater-card" style={{ marginTop: 16 }}>
                  <div className="backstage-preset-head">
                    <strong>软件版本与自动更新（Tauri v2 签名校验）</strong>
                    <span
                      className={`mini-badge${
                        updater.state.status === "available" || updater.state.status === "installed"
                          ? " is-ok"
                          : updater.state.status === "error"
                            ? " badge-err"
                            : ""
                      }`}
                    >
                      {updater.state.supported
                        ? updater.state.currentVersion
                          ? `v${updater.state.currentVersion}`
                          : "桌面端"
                        : "浏览器模式"}
                    </span>
                  </div>
                  <p className="backstage-updater-msg">{updater.state.message}</p>
                  {updater.state.releaseNotes && updater.state.status === "available" && (
                    <pre className="backstage-updater-notes">{updater.state.releaseNotes}</pre>
                  )}
                  {updater.state.status === "downloading" && (
                    <div className="backstage-updater-progress" role="progressbar" aria-valuenow={updater.state.progressPercent ?? 0} aria-valuemin={0} aria-valuemax={100}>
                      <div
                        className="backstage-updater-progress-bar"
                        style={{ width: `${updater.state.progressPercent ?? 15}%` }}
                      />
                    </div>
                  )}
                  <div className="backstage-updater-actions">
                    <button
                      type="button"
                      className="btn-secondary"
                      disabled={!updater.state.supported || updater.state.busy}
                      onClick={() => void updater.checkForUpdates()}
                    >
                      {updater.state.status === "checking" ? "正在检查更新…" : "检查更新"}
                    </button>
                    {updater.state.status === "available" && (
                      <button
                        type="button"
                        className="btn-primary"
                        disabled={updater.state.busy}
                        onClick={() => void updater.installUpdate()}
                      >
                        下载并安装 v{updater.state.availableVersion}
                      </button>
                    )}
                  </div>
                </div>
              )}
            </section>
          )}
        </div>
      </div>
    </div>
  );
}
