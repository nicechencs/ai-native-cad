import { useEffect, useRef, useState, type ReactNode } from "react";
import {
  isCommandEnabled,
  type CommandContext,
  type CommandDefinition,
} from "../../core/commands/commandRegistry.ts";
import {
  ROLE_PRESETS,
  type LayoutPreset,
  type ThemeMode,
} from "../../core/layout/layoutStore.ts";
import {
  loadWorkbenchUiPrefs,
  updateWorkbenchUiPrefs,
  type PersistedRibbonTab,
} from "../../core/layout/workbenchPrefsStore.ts";
import { WorkbenchTabPanel, WorkbenchTabs } from "../../components/WorkbenchTabs.tsx";
import { experimentalTracksEnabled } from "../../core/session/experimentalTracks.ts";
import { CadIcon, type CadIconName } from "../icons/CadIcons.tsx";

export type RibbonTabId = PersistedRibbonTab | "sketch" | "drawing";

export interface RibbonTabMeta {
  readonly id: RibbonTabId;
  readonly label: string;
  readonly contextual?: "sketch" | "drawing";
}

export const RIBBON_TABS: readonly RibbonTabMeta[] = [
  { id: "model", label: "建模" },
  { id: "assembly", label: "装配" },
  { id: "analysis", label: "分析与验证" },
  { id: "annotate", label: "标注与工程图" },
  { id: "view", label: "视图" },
  { id: "tools", label: "工具与代码" },
  { id: "ai", label: "AI 协作" },
] as const;

export interface RibbonBarProps {
  readonly commandMap: ReadonlyMap<string, CommandDefinition>;
  readonly context: CommandContext;
  readonly onExecute: (commandId: string) => void;
  readonly onOpenFileBackstage: () => void;
  readonly documentName: string;
  readonly partName: string;
  readonly activeBranch: string;
  readonly compiling: boolean;
  readonly hasError: boolean;
  readonly runtimeBadge: string;
  readonly runtimeTone: "host" | "demo" | "native";
  readonly theme: ThemeMode;
  readonly activeRole: LayoutPreset;
  readonly onApplyPreset: (preset: LayoutPreset) => void;
  readonly sketchActive?: boolean;
  readonly drawingActive?: boolean;
}

interface RibbonCommandButtonProps {
  readonly commandId: string;
  readonly label: string;
  readonly icon: CadIconName;
  readonly size?: "large" | "compact";
  readonly variant?: "default" | "primary" | "ai" | "sketch";
  readonly commandMap: ReadonlyMap<string, CommandDefinition>;
  readonly context: CommandContext;
  readonly onExecute: (commandId: string) => void;
}

function RibbonCommandButton({
  commandId,
  label,
  icon,
  size = "large",
  variant = "default",
  commandMap,
  context,
  onExecute,
}: RibbonCommandButtonProps) {
  const cmd = commandMap.get(commandId);
  const enabled = cmd ? isCommandEnabled(cmd, context) : true;
  const active = cmd?.isActive?.(context) ?? false;
  const tooltip = cmd
    ? `${cmd.title}${cmd.shortcut ? ` (${cmd.shortcut})` : ""}${!enabled && cmd.disabledReason ? ` · ${cmd.disabledReason(context)}` : ""}`
    : label;

  const variantClass =
    variant === "ai"
      ? " ribbon-btn-ai"
      : variant === "primary"
        ? " ribbon-btn-primary"
        : variant === "sketch"
          ? " ribbon-btn-sketch"
          : "";

  return (
    <button
      type="button"
      className={`ribbon-btn ribbon-btn-${size}${variantClass}${active ? " is-active" : ""}`}
      disabled={!enabled}
      aria-pressed={cmd?.isActive ? active : undefined}
      title={tooltip}
      onClick={() => onExecute(commandId)}
    >
      <CadIcon name={icon} size={size === "large" ? 18 : 14} />
      <span className="ribbon-btn-label">{label}</span>
    </button>
  );
}

function QuickAccessButton({
  commandId,
  icon,
  commandMap,
  context,
  onExecute,
}: {
  readonly commandId: string;
  readonly icon: CadIconName;
  readonly commandMap: ReadonlyMap<string, CommandDefinition>;
  readonly context: CommandContext;
  readonly onExecute: (commandId: string) => void;
}) {
  const cmd = commandMap.get(commandId);
  const enabled = cmd ? isCommandEnabled(cmd, context) : true;
  const tooltip = cmd
    ? `${cmd.title}${cmd.shortcut ? ` (${cmd.shortcut})` : ""}${!enabled && cmd.disabledReason ? ` · ${cmd.disabledReason(context)}` : ""}`
    : commandId;

  return (
    <button
      type="button"
      className="icon-ghost-btn"
      disabled={!enabled}
      title={tooltip}
      aria-label={cmd?.title ?? commandId}
      onClick={() => onExecute(commandId)}
    >
      <CadIcon name={icon} size={14} />
    </button>
  );
}

function RibbonGroup({
  label,
  children,
}: {
  readonly label: string;
  readonly children: ReactNode;
}) {
  return (
    <section className="ribbon-group" role="group" aria-label={label}>
      <div className="ribbon-group-body">{children}</div>
      <span className="ribbon-group-title" aria-hidden="true">
        {label}
      </span>
    </section>
  );
}

function RibbonStack({ children }: { readonly children: ReactNode }) {
  return <div className="ribbon-stack">{children}</div>;
}

export function RibbonBar({
  commandMap,
  context,
  onExecute,
  onOpenFileBackstage,
  documentName,
  partName,
  activeBranch,
  compiling,
  hasError,
  runtimeBadge,
  runtimeTone,
  theme,
  activeRole,
  onApplyPreset,
  sketchActive = false,
  drawingActive = false,
}: RibbonBarProps) {
  const [storedTab, setStoredTab] = useState<PersistedRibbonTab>(() => loadWorkbenchUiPrefs().ribbonTab);
  const [contextualTab, setContextualTab] = useState<"sketch" | "drawing" | null>(null);
  const [expanded, setExpanded] = useState(() => loadWorkbenchUiPrefs().ribbonExpanded);
  const [roleMenuOpen, setRoleMenuOpen] = useState(false);
  const roleMenuRef = useRef<HTMLDivElement>(null);

  // Automatically surface dynamic contextual tabs like Creo/NX when entering Sketch or 2D Drawing mode.
  useEffect(() => {
    if (sketchActive) {
      setContextualTab("sketch");
      setExpanded(true);
    } else if (drawingActive) {
      setContextualTab("drawing");
    } else {
      setContextualTab(null);
    }
  }, [sketchActive, drawingActive]);

  useEffect(() => {
    if (!roleMenuOpen) return;
    const onPointerDown = (event: PointerEvent) => {
      if (roleMenuRef.current && !roleMenuRef.current.contains(event.target as Node)) {
        setRoleMenuOpen(false);
      }
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setRoleMenuOpen(false);
    };
    window.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [roleMenuOpen]);

  const visibleTabs: readonly RibbonTabMeta[] = [
    ...RIBBON_TABS,
    ...(sketchActive ? [{ id: "sketch" as const, label: "草图", contextual: "sketch" as const }] : []),
    ...(drawingActive ? [{ id: "drawing" as const, label: "2D 工程图", contextual: "drawing" as const }] : []),
  ];

  const activeTab: RibbonTabId = contextualTab ?? storedTab;

  const selectRibbonTab = (tab: RibbonTabId) => {
    if (tab === "sketch" || tab === "drawing") {
      setContextualTab(tab);
      setExpanded(true);
      return;
    }
    setContextualTab(null);
    setStoredTab(tab);
    setExpanded(true);
    updateWorkbenchUiPrefs({ ribbonTab: tab, ribbonExpanded: true });
  };

  const toggleExpanded = () => {
    setExpanded((prev) => {
      const next = !prev;
      updateWorkbenchUiPrefs({ ribbonExpanded: next });
      return next;
    });
  };

  const activeRoleMeta = ROLE_PRESETS.find((r) => r.id === activeRole) ?? ROLE_PRESETS[0];

  const btn = (
    commandId: string,
    label: string,
    icon: CadIconName,
    options?: { readonly size?: "large" | "compact"; readonly variant?: "default" | "primary" | "ai" | "sketch" },
  ) => (
    <RibbonCommandButton
      commandId={commandId}
      label={label}
      icon={icon}
      size={options?.size}
      variant={options?.variant}
      commandMap={commandMap}
      context={context}
      onExecute={onExecute}
    />
  );

  return (
    <header className="studio-ribbon" role="banner">
      <div className="ribbon-top-bar">
        <div className="ribbon-top-left">
          <div className="studio-brand" title="AI-Native CAD Studio (NX / Creo Industrial UX)">
            <span className="brand-icon-wrap">
              <CadIcon name="logo" size={15} />
            </span>
            <span className="brand-name">CAD Studio</span>
          </div>

          <button
            type="button"
            className="ribbon-file-btn"
            aria-haspopup="dialog"
            title="打开文件后台菜单（新建 / 打开 / 保存 / 导出 / 偏好）"
            onClick={onOpenFileBackstage}
          >
            <CadIcon name="fileMenu" size={13} />
            <span>文件</span>
          </button>

          <div className="ribbon-qat" role="group" aria-label="快速访问工具栏">
            <QuickAccessButton commandId="file.downloadSource" icon="export" commandMap={commandMap} context={context} onExecute={onExecute} />
            <QuickAccessButton commandId="edit.undo" icon="undo" commandMap={commandMap} context={context} onExecute={onExecute} />
            <QuickAccessButton commandId="edit.redo" icon="redo" commandMap={commandMap} context={context} onExecute={onExecute} />
            <QuickAccessButton commandId="model.regenerate" icon="refresh" commandMap={commandMap} context={context} onExecute={onExecute} />
          </div>

          <div className="ribbon-top-divider" aria-hidden="true" />

          <WorkbenchTabs
            prefix="ribbon"
            label="功能区标签页"
            tabs={visibleTabs}
            active={activeTab}
            onSelect={selectRibbonTab}
            className="ribbon-tabs"
            tabClassName={(tab, active) =>
              `ribbon-tab${active && expanded ? " is-active" : ""}${tab.id === "ai" ? " is-ai" : ""}${tab.contextual === "sketch" ? " is-contextual-sketch" : ""}${tab.contextual === "drawing" ? " is-contextual-drawing" : ""}`
            }
          />
        </div>

        <div className="ribbon-top-center">
          <div className="doc-pill" title={`当前文档: ${documentName} · 分支: ${activeBranch}`}>
            <span
              className={`doc-status-dot${compiling ? " is-compiling" : hasError ? " is-error" : " is-ok"}`}
            />
            <span className="doc-name">{partName || documentName}</span>
            <span className="doc-branch-chip">{activeBranch}</span>
          </div>

          <button
            type="button"
            className="palette-trigger"
            onClick={() => onExecute("command.openPalette")}
            title="NX Command Finder · 搜索命令、特征或参数 (Ctrl+K)"
          >
            <CadIcon name="search" size={13} />
            <span>Command Finder：搜索命令、特征或参数…</span>
            <kbd className="shortcut-kbd">Ctrl+K</kbd>
          </button>
        </div>

        <div className="ribbon-top-right">
          {/* NX-inspired Roles switcher */}
          <div ref={roleMenuRef} className="role-switcher-anchor">
            <button
              type="button"
              className={`role-switcher-btn${roleMenuOpen ? " is-open" : ""}`}
              aria-haspopup="menu"
              aria-expanded={roleMenuOpen}
              title={`当前工作角色：${activeRoleMeta.label}`}
              onClick={() => setRoleMenuOpen((cur) => !cur)}
            >
              <CadIcon name="layout" size={13} />
              <span>{activeRoleMeta.label.split(" ")[0]}</span>
              <CadIcon name="chevronDown" size={11} />
            </button>
            {roleMenuOpen && (
              <div className="studio-dropdown role-dropdown" role="menu" aria-label="工作角色与布局预设">
                <div className="context-menu-title">工作角色预设 (NX Roles)</div>
                {ROLE_PRESETS.map((role) => (
                  <button
                    key={role.id}
                    type="button"
                    role="menuitemradio"
                    aria-checked={activeRole === role.id}
                    className={`dropdown-item${activeRole === role.id ? " is-checked" : ""}`}
                    onClick={() => {
                      onApplyPreset(role.id);
                      setRoleMenuOpen(false);
                    }}
                  >
                    <span className="dropdown-item-check">
                      {activeRole === role.id ? <CadIcon name="check" size={13} /> : <CadIcon name="layout" size={13} />}
                    </span>
                    <span className="role-item-text">
                      <strong>{role.label}</strong>
                      <small>{role.description}</small>
                    </span>
                  </button>
                ))}
              </div>
            )}
          </div>

          <button
            type="button"
            className="icon-ghost-btn"
            onClick={() => onExecute("theme.toggle")}
            title={theme === "dark" ? "切换到浅色蓝图主题 (Ctrl+Shift+T)" : "切换到深色工业主题 (Ctrl+Shift+T)"}
            aria-label="切换主题"
          >
            <CadIcon name={theme === "dark" ? "sun" : "moon"} size={15} />
          </button>

          <span className={`runtime-badge runtime-${runtimeTone}`}>{runtimeBadge}</span>

          <button
            type="button"
            className={`icon-ghost-btn ribbon-collapse-btn${expanded ? "" : " is-collapsed"}`}
            aria-controls="studio-ribbon-strip"
            aria-expanded={expanded}
            title={expanded ? "收起功能区" : "展开功能区"}
            aria-label={expanded ? "收起功能区" : "展开功能区"}
            onClick={toggleExpanded}
          >
            <CadIcon name="chevronDown" size={14} />
          </button>
        </div>
      </div>

      <div id="studio-ribbon-strip" className="ribbon-command-bar" hidden={!expanded}>
        {/* 1. 建模 (Model) — Strict Creo workflow order: Datum -> Sketch -> Shapes -> Engineering -> Edit/Boolean -> Direct */}
        <WorkbenchTabPanel prefix="ribbon" id="model" active={activeTab === "model"} className="ribbon-panel">
          <RibbonGroup label="基准">
            <RibbonStack>
              {btn("model.sketch.xy", "XY 基准草图", "datumPlane", { size: "compact" })}
              {btn("model.sketch.xz", "XZ 基准草图", "datumPlane", { size: "compact" })}
            </RibbonStack>
          </RibbonGroup>

          <RibbonGroup label="草图">
            {btn("model.sketch", "新建草图", "sketch", { variant: "primary" })}
          </RibbonGroup>

          <RibbonGroup label="基础成形">
            {btn("model.extrude", "拉伸", "extrude")}
            {btn("model.revolve", "旋转", "revolve")}
            <RibbonStack>
              {btn("model.primitiveBox", "长方体", "boxPrimitive", { size: "compact" })}
              {btn("model.primitiveCylinder", "圆柱体", "cylinderPrimitive", { size: "compact" })}
            </RibbonStack>
          </RibbonGroup>

          <RibbonGroup label="工程修饰">
            {btn("model.throughHole", "孔 / 标准孔", "throughHole")}
            {btn("model.counterboreHole", "沉头孔", "counterbore")}
            {btn("model.filletEdges", "边缘圆角", "fillet")}
            {btn("model.chamferEdges", "边缘倒角", "chamfer")}
            <RibbonStack>
              {btn("model.shell", "薄壁抽壳", "shell", { size: "compact" })}
              {btn("model.draft", "拔模斜度", "draft", { size: "compact" })}
            </RibbonStack>
          </RibbonGroup>

          <RibbonGroup label="变换与布尔">
            {btn("model.pattern", "特征阵列", "pattern")}
            <RibbonStack>
              {btn("model.mirror", "对称镜像", "mirror", { size: "compact" })}
              {btn("model.boolean", "布尔运算", "booleanOp", { size: "compact" })}
            </RibbonStack>
          </RibbonGroup>

          <RibbonGroup label="同步直接编辑">
            {btn("model.directEdit", "偏移/移动面", "directEdit")}
          </RibbonGroup>

          {experimentalTracksEnabled() ? (
            <RibbonGroup label="实验轨道">
              {btn("experimental.subd", "SubD", "cube")}
              {btn("experimental.tpms", "TPMS", "cube")}
              {btn("experimental.gpuSlang", "GPU Slang", "cube")}
            </RibbonGroup>
          ) : null}

          <RibbonGroup label="编辑与再生">
            <RibbonStack>
              {btn("model.regenerate", "再生重建", "refresh", { size: "compact" })}
              {btn("source.focusSelection", "定位源码", "code", { size: "compact" })}
            </RibbonStack>
          </RibbonGroup>
        </WorkbenchTabPanel>

        {/* 2. 装配 (Assembly) */}
        <WorkbenchTabPanel prefix="ribbon" id="assembly" active={activeTab === "assembly"} className="ribbon-panel">
          <RibbonGroup label="组件管理">
            {btn("file.importAsset", "插入外部件", "assetImport", { variant: "primary" })}
            {btn("panel.openAssembly", "装配导航树", "assembly")}
          </RibbonGroup>
          <RibbonGroup label="6-DOF 装配约束">
            {btn("assembly.addMate", "添加配合", "mate")}
          </RibbonGroup>
          <RibbonGroup label="装配审查">
            {btn("view.toggleExplode", "爆炸视图", "explode")}
            <RibbonStack>
              {btn("display.isolate", "孤立组件", "isolate", { size: "compact" })}
              {btn("display.toggleTranslucent", "组件半透明", "translucent", { size: "compact" })}
            </RibbonStack>
            {btn("display.showAll", "显示全部", "eye")}
          </RibbonGroup>
        </WorkbenchTabPanel>

        {/* 3. 分析与验证 (Analysis) */}
        <WorkbenchTabPanel prefix="ribbon" id="analysis" active={activeTab === "analysis"} className="ribbon-panel">
          <RibbonGroup label="几何测量">
            {btn("inspect.measureCircle", "圆心距", "measureCircle")}
            {btn("inspect.measureFace", "平行面间距", "measureFace")}
            {btn("inspect.clearMeasurement", "清除测量", "trash")}
          </RibbonGroup>
          <RibbonGroup label="L0–L4 三值门禁">
            {btn("model.regenerate", "运行验证", "shieldCheck", { variant: "primary" })}
            {btn("analysis.addAssertion", "设计断言", "assertGate")}
            {btn("panel.openChecks", "诊断与契约", "shieldCheck")}
          </RibbonGroup>
          <RibbonGroup label="剖切与质量属性">
            {btn("view.toggleSectionCut", "实时剖切", "sectionCut")}
            {btn("view.toggleKeepInBox", "Keep-In 包络", "boundingBox")}
            {btn("panel.openProperties", "质量与属性", "properties")}
          </RibbonGroup>
        </WorkbenchTabPanel>

        {/* 4. 标注与工程图 (Annotate & Drawing) */}
        <WorkbenchTabPanel prefix="ribbon" id="annotate" active={activeTab === "annotate"} className="ribbon-panel">
          <RibbonGroup label="三维语义与 PMI">
            {btn("annotate.tagTopology", "语义拓扑打标", "tag", { variant: "primary" })}
            {btn("export.step", "导出 STEP AP242 (PMI)", "export")}
          </RibbonGroup>
          <RibbonGroup label="2D 关联工程图">
            {btn("view.modeDrawing", "切换 2D 工程图", "drawing2d")}
            <RibbonStack>
              {btn("export.drawing", "导出工程图 SVG", "drawing2d", { size: "compact" })}
              {btn("export.dxf", "导出切割 DXF", "export", { size: "compact" })}
            </RibbonStack>
          </RibbonGroup>
        </WorkbenchTabPanel>

        {/* 5. 视图 (View) */}
        <WorkbenchTabPanel prefix="ribbon" id="view" active={activeTab === "view"} className="ribbon-panel">
          <RibbonGroup label="视图模式">
            {btn("view.mode3d", "三维视图", "view3d")}
            {btn("view.modeDrawing", "2D 工程图", "drawing2d")}
          </RibbonGroup>
          <RibbonGroup label="显示样式">
            <RibbonStack>
              {btn("display.style.shadedEdges", "带边着色", "cube", { size: "compact" })}
              {btn("display.style.shaded", "纯着色", "shaded", { size: "compact" })}
            </RibbonStack>
            <RibbonStack>
              {btn("display.style.translucent", "半透明", "translucent", { size: "compact" })}
              {btn("display.style.wireframe", "线框", "wireframe", { size: "compact" })}
            </RibbonStack>
            {btn("display.style.hlr", "技术线稿", "drawing2d")}
          </RibbonGroup>
          <RibbonGroup label="标准相机视角">
            {btn("view.preset.iso", "等轴测", "viewIso")}
            <RibbonStack>
              {btn("view.preset.front", "主视", "viewFront", { size: "compact" })}
              {btn("view.preset.back", "后视", "viewFront", { size: "compact" })}
            </RibbonStack>
            <RibbonStack>
              {btn("view.preset.top", "俯视", "viewTop", { size: "compact" })}
              {btn("view.preset.bottom", "仰视", "viewTop", { size: "compact" })}
            </RibbonStack>
            <RibbonStack>
              {btn("view.preset.right", "右视", "viewRight", { size: "compact" })}
              {btn("view.preset.left", "左视", "viewRight", { size: "compact" })}
            </RibbonStack>
            <RibbonStack>
              {btn("view.fit", "适应窗口", "fitView", { size: "compact" })}
              {btn("view.normalTo", "正视选中面", "normalTo", { size: "compact" })}
            </RibbonStack>
            <RibbonStack>
              {btn("view.zoomIn", "放大", "zoomIn", { size: "compact" })}
              {btn("view.zoomOut", "缩小", "zoomOut", { size: "compact" })}
            </RibbonStack>
            <RibbonStack>
              {btn("view.rotateLeft", "左转90°", "rotateLeft", { size: "compact" })}
              {btn("view.rotateRight", "右转90°", "rotateRight", { size: "compact" })}
            </RibbonStack>
            {btn("display.toggleProjection", context.projection === "perspective" ? "透视投影" : "正交投影", context.projection === "perspective" ? "perspective" : "orthographic")}
          </RibbonGroup>
          <RibbonGroup label="显示辅助">
            <RibbonStack>
              {btn("view.toggleKeepInBox", "包络盒", "boundingBox", { size: "compact" })}
              {btn("view.toggleSectionCut", "实时剖切", "sectionCut", { size: "compact" })}
            </RibbonStack>
            <RibbonStack>
              {btn("display.showAll", "显示全部", "eye", { size: "compact" })}
              {btn("view.toggleExplode", "爆炸视图", "explode", { size: "compact" })}
            </RibbonStack>
          </RibbonGroup>
          <RibbonGroup label="工作区与角色">
            <RibbonStack>
              {btn("layout.preset.standard", "机械设计", "layout", { size: "compact" })}
              {btn("layout.preset.code", "代码协同", "code", { size: "compact" })}
            </RibbonStack>
            <RibbonStack>
              {btn("layout.preset.ai", "AI 创作", "sparkles", { size: "compact" })}
              {btn("layout.toggleZen", "纯净视口", "maximize", { size: "compact" })}
            </RibbonStack>
            <RibbonStack>
              {btn("layout.toggleLeft", "左侧导航栏", "sidebarLeft", { size: "compact" })}
              {btn("layout.toggleRight", "右侧抽屉", "sidebarRight", { size: "compact" })}
            </RibbonStack>
            {btn("theme.toggle", theme === "dark" ? "浅色蓝图" : "深色工业", theme === "dark" ? "sun" : "moon")}
          </RibbonGroup>
        </WorkbenchTabPanel>

        {/* 6. 工具与代码 (Tools) */}
        <WorkbenchTabPanel prefix="ribbon" id="tools" active={activeTab === "tools"} className="ribbon-panel">
          <RibbonGroup label="参数与契约">
            {btn("panel.openProperties", "参数与属性", "properties")}
            {btn("panel.openChecks", "设计契约", "shieldCheck")}
          </RibbonGroup>
          <RibbonGroup label="代码与双向投影">
            {btn("panel.openSource", "KCL 源码视图", "code", { variant: "primary" })}
            <RibbonStack>
              {btn("source.focusSelection", "定位选中源码", "code", { size: "compact" })}
              {btn("file.downloadSource", "下载 .kcl", "export", { size: "compact" })}
            </RibbonStack>
          </RibbonGroup>
          <RibbonGroup label="导航键位方案">
            <RibbonStack>
              {btn("nav.preset.nxCreo", "NX / Creo 标准", "mouse", { size: "compact" })}
              {btn("nav.preset.solidworks", "SolidWorks", "mouse", { size: "compact" })}
            </RibbonStack>
            {btn("nav.preset.web", "Web / 触控板", "mouse")}
          </RibbonGroup>
          <RibbonGroup label="命令与帮助">
            {btn("command.openPalette", "Command Finder", "command")}
            {btn("help.shortcuts", "快捷键速查", "keyboard")}
          </RibbonGroup>
        </WorkbenchTabPanel>

        {/* 7. AI 协作 (AI Studio) */}
        <WorkbenchTabPanel prefix="ribbon" id="ai" active={activeTab === "ai"} className="ribbon-panel">
          <RibbonGroup label="AI 生成与编辑">
            {btn("ai.openGenerate", "AI 0→1 新建零件", "sparkles", { variant: "ai" })}
            {btn("ai.openEdit", "AI 1→N 增量修改", "sparkles", { variant: "ai" })}
            {btn("ai.inlinePrompt", "视口内联指令", "sparkles")}
          </RibbonGroup>
          <RibbonGroup label="任务控制">
            {btn("ai.editSelected", "修改选中特征", "sparkles")}
            {btn("ai.cancel", "中止任务", "close")}
          </RibbonGroup>
        </WorkbenchTabPanel>

        {/* Contextual Tab A: Sketch (Creo/NX dynamic green tab) */}
        {sketchActive && (
          <WorkbenchTabPanel prefix="ribbon" id="sketch" active={activeTab === "sketch"} className="ribbon-panel">
            <RibbonGroup label="草图视图">
              {btn("view.normalTo", "正视草图平面", "normalTo", { variant: "sketch" })}
            </RibbonGroup>
            <RibbonGroup label="2D 图元 (cad-gcs)">
              {btn("sketch.addRect", "矩形", "boxPrimitive")}
              {btn("sketch.addCircle", "圆", "measureCircle")}
              <RibbonStack>
                {btn("sketch.addLine", "直线", "wireframe", { size: "compact" })}
                {btn("sketch.addArc", "圆弧", "fillet", { size: "compact" })}
              </RibbonStack>
            </RibbonGroup>
            <RibbonGroup label="几何与尺寸约束">
              <RibbonStack>
                {btn("sketch.constrainHorizontal", "水平 / 竖直", "flip", { size: "compact" })}
                {btn("sketch.constrainDimension", "距离 / 直径", "measureFace", { size: "compact" })}
              </RibbonStack>
            </RibbonGroup>
            <RibbonGroup label="草图完成">
              {btn("dashboard.confirm", "完成草图 ✓", "check", { variant: "primary" })}
              {btn("dashboard.cancel", "放弃草图", "close")}
            </RibbonGroup>
          </WorkbenchTabPanel>
        )}

        {/* Contextual Tab B: 2D Drawing */}
        {drawingActive && (
          <WorkbenchTabPanel prefix="ribbon" id="drawing" active={activeTab === "drawing"} className="ribbon-panel">
            <RibbonGroup label="视图切换">
              {btn("view.mode3d", "返回 3D 模型", "view3d", { variant: "primary" })}
            </RibbonGroup>
            <RibbonGroup label="工程图交付">
              {btn("export.drawing", "导出 HLR SVG", "drawing2d")}
              {btn("export.dxf", "导出 DXF", "export")}
            </RibbonGroup>
          </WorkbenchTabPanel>
        )}
      </div>
    </header>
  );
}
