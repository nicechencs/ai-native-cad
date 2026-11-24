import { useEffect, useRef, useState } from "react";
import {
  BUILTIN_DISPLAY_PRESETS,
  countHiddenEntities,
  countTranslucentEntities,
  DISPLAY_CATEGORY_META,
  hasActiveDisplayOverrides,
  VIEW_PRESET_OPTIONS,
  VISUAL_STYLE_OPTIONS,
  type DisplayCategoryKey,
  type DisplayStatePreset,
  type SavedDisplayState,
  type ViewportDisplayState,
  type ViewPreset,
  type VisualStyle,
} from "../../core/viewport/displayState.ts";
import type { SectionCutState, SelectionTarget } from "../../types/scene.ts";
import type { MeasurementMode } from "../../utils/viewportMeasurement.ts";
import { CadIcon, type CadIconName } from "../../ui/icons/CadIcons.tsx";
import { ViewCube } from "./ViewCube.tsx";
import { TURN_LEFT_YAW, TURN_RIGHT_YAW } from "./viewOrientation.ts";

export interface ViewportHudOverlayProps {
  readonly viewportTab: "3d" | "drawing";
  readonly onSelectViewportTab: (tab: "3d" | "drawing") => void;
  readonly onApplyPreset: (preset: ViewPreset) => void;
  readonly onFitView: () => void;
  readonly onNormalToSelection?: () => void;
  readonly onZoomBy?: (factor: number) => void;
  readonly onRotateStep?: (deltaYaw: number) => void;
  readonly onOrbitBy?: (deltaYaw: number, deltaPitch: number) => void;
  readonly cameraOrientation: { readonly yaw: number; readonly pitch: number };
  readonly displayState: ViewportDisplayState;
  readonly onChangeVisualStyle: (style: VisualStyle) => void;
  readonly onToggleProjection: () => void;
  readonly onToggleCategory: (key: DisplayCategoryKey) => void;
  readonly onResetCategories: () => void;
  readonly onApplyDisplayPreset: (preset: DisplayStatePreset) => void;
  readonly savedDisplayStates?: readonly SavedDisplayState[];
  readonly onSaveDisplayState?: (name: string) => void;
  readonly onRestoreDisplayState?: (saved: SavedDisplayState) => void;
  readonly onDeleteDisplayState?: (id: string) => void;
  readonly availableSemanticTags?: readonly string[];
  readonly onSelectSemanticFilter?: (tag: string | null) => void;
  readonly onExitIsolate: () => void;
  readonly onShowAllEntities: () => void;
  readonly onClearAllOverrides: () => void;
  readonly selectedTarget: SelectionTarget;
  readonly measurementMode: MeasurementMode | null;
  readonly onExitMeasurement: () => void;
  readonly sectionCut: SectionCutState;
  readonly onChangeSectionCut: (next: SectionCutState) => void;
  readonly isAssembly: boolean;
  readonly explodeFactor: number;
  readonly onChangeExplodeFactor: (value: number) => void;
  readonly onExecuteCommand: (commandId: string) => void;
}

function visualStyleIcon(style: VisualStyle): CadIconName {
  switch (style) {
    case "shaded-edges":
      return "cube";
    case "shaded":
      return "shaded";
    case "translucent":
      return "translucent";
    case "wireframe":
      return "wireframe";
    case "hlr":
      return "drawing2d";
  }
}

/**
 * PTC Creo style In-Graphics Toolbar + ViewCube + Section/Explode sliders.
 * Selection filters now live permanently in TopBorderBar, and quick selection actions
 * live in the cursor-adjacent MiniToolbar — keeping the 3D viewport clean and unobstructed.
 */
export function ViewportHudOverlay({
  viewportTab,
  onSelectViewportTab,
  onApplyPreset,
  onFitView,
  onNormalToSelection,
  onZoomBy,
  onRotateStep,
  onOrbitBy,
  cameraOrientation,
  displayState,
  onChangeVisualStyle,
  onToggleProjection,
  onToggleCategory,
  onResetCategories,
  onApplyDisplayPreset,
  savedDisplayStates = [],
  onSaveDisplayState,
  onRestoreDisplayState,
  onDeleteDisplayState,
  availableSemanticTags = [],
  onSelectSemanticFilter,
  onExitIsolate,
  onShowAllEntities,
  onClearAllOverrides,
  selectedTarget,
  sectionCut,
  onChangeSectionCut,
  isAssembly,
  explodeFactor,
  onChangeExplodeFactor,
}: ViewportHudOverlayProps) {
  const [openMenu, setOpenMenu] = useState<"style" | "visibility" | "camera" | null>(null);
  const [newStateName, setNewStateName] = useState("");
  const hudBarRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!openMenu) return;
    const handlePointerDown = (event: PointerEvent) => {
      if (hudBarRef.current && event.target instanceof Node && !hudBarRef.current.contains(event.target)) {
        setOpenMenu(null);
      }
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpenMenu(null);
    };
    window.addEventListener("pointerdown", handlePointerDown);
    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("pointerdown", handlePointerDown);
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [openMenu]);

  const currentStyleMeta =
    VISUAL_STYLE_OPTIONS.find((item) => item.id === displayState.visualStyle) ?? VISUAL_STYLE_OPTIONS[0];
  const hiddenCount = countHiddenEntities(displayState);
  const translucentCount = countTranslucentEntities(displayState);
  const hasOverrides = hasActiveDisplayOverrides(displayState);

  return (
    <>
      {/* Top-Left: 3D / 2D Drawing Viewport Switcher */}
      <div className="viewport-hud-top-left" role="tablist" aria-label="视口视图切换">
        <button
          type="button"
          role="tab"
          aria-selected={viewportTab === "3d"}
          className={`hud-seg-btn${viewportTab === "3d" ? " is-active" : ""}`}
          onClick={() => onSelectViewportTab("3d")}
        >
          <CadIcon name="view3d" size={14} />
          <span>三维模型</span>
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={viewportTab === "drawing"}
          className={`hud-seg-btn${viewportTab === "drawing" ? " is-active" : ""}`}
          onClick={() => onSelectViewportTab("drawing")}
        >
          <CadIcon name="drawing2d" size={14} />
          <span>2D 工程图</span>
        </button>
      </div>

      {/* Top-Center: Clean Creo-style In-Graphics Toolbar (Visual Display Only) */}
      {viewportTab === "3d" && (
        <div
          ref={hudBarRef}
          className="viewport-hud-headsup"
          role="toolbar"
          aria-label="3D 显示控制栏"
        >
          {/* Visual Style & Named Display States (View Manager) */}
          <div className="hud-dropdown-anchor">
            <button
              type="button"
              className={`hud-view-btn${openMenu === "style" ? " is-active" : ""}`}
              aria-expanded={openMenu === "style"}
              aria-haspopup="menu"
              title={`显示样式：${currentStyleMeta.label}`}
              onClick={() => setOpenMenu((cur) => (cur === "style" ? null : "style"))}
            >
              <CadIcon name={visualStyleIcon(displayState.visualStyle)} size={13} />
              <span>{currentStyleMeta.shortLabel}</span>
              <CadIcon name="chevronDown" size={11} />
            </button>

            {openMenu === "style" && (
              <div className="hud-popover-menu" role="menu" aria-label="显示样式与预设">
                <div className="hud-popover-section-title">视觉样式 (L0)</div>
                {VISUAL_STYLE_OPTIONS.map((opt) => (
                  <button
                    key={opt.id}
                    type="button"
                    role="menuitemradio"
                    aria-checked={displayState.visualStyle === opt.id}
                    className={`hud-popover-item${displayState.visualStyle === opt.id ? " is-selected" : ""}`}
                    onClick={() => {
                      onChangeVisualStyle(opt.id);
                      setOpenMenu(null);
                    }}
                  >
                    <CadIcon name={visualStyleIcon(opt.id)} size={14} />
                    <div className="hud-popover-item-text">
                      <strong>{opt.label}</strong>
                      <span>{opt.description}</span>
                    </div>
                    {displayState.visualStyle === opt.id && <CadIcon name="check" size={13} />}
                  </button>
                ))}
                <div className="hud-popover-divider" role="separator" />
                <div className="hud-popover-section-title">快速显示方案 (View Manager)</div>
                <div className="hud-preset-grid">
                  {BUILTIN_DISPLAY_PRESETS.map((preset) => (
                    <button
                      key={preset.id}
                      type="button"
                      className="hud-preset-chip"
                      onClick={() => {
                        onApplyDisplayPreset(preset);
                        setOpenMenu(null);
                      }}
                    >
                      {preset.name}
                    </button>
                  ))}
                </div>
                {onSaveDisplayState && (
                  <>
                    <div className="hud-popover-divider" role="separator" />
                    <div className="hud-popover-section-title">命名显示状态 (Display States)</div>
                    {savedDisplayStates.length > 0 && (
                      <div className="hud-saved-states-list">
                        {savedDisplayStates.map((saved) => (
                          <div key={saved.id} className="hud-saved-state-row">
                            <button
                              type="button"
                              className="hud-saved-state-btn"
                              onClick={() => {
                                onRestoreDisplayState?.(saved);
                                setOpenMenu(null);
                              }}
                            >
                              <span>{saved.name}</span>
                            </button>
                            {onDeleteDisplayState && (
                              <button
                                type="button"
                                className="icon-ghost-btn"
                                aria-label={`删除显示状态 ${saved.name}`}
                                onClick={() => onDeleteDisplayState(saved.id)}
                              >
                                <CadIcon name="close" size={11} />
                              </button>
                            )}
                          </div>
                        ))}
                      </div>
                    )}
                    <form
                      className="hud-save-state-form"
                      onSubmit={(event) => {
                        event.preventDefault();
                        const label = newStateName.trim() || `状态 ${savedDisplayStates.length + 1}`;
                        onSaveDisplayState(label);
                        setNewStateName("");
                      }}
                    >
                      <input
                        type="text"
                        className="hud-save-state-input"
                        placeholder="保存当前显示状态…"
                        aria-label="命名显示状态名称"
                        value={newStateName}
                        onChange={(event) => setNewStateName(event.target.value)}
                      />
                      <button type="submit" className="hud-preset-chip">
                        保存
                      </button>
                    </form>
                  </>
                )}
              </div>
            )}
          </div>

          <div className="hud-bar-divider" aria-hidden="true" />

          {/* Orthographic / Perspective Toggle */}
          <button
            type="button"
            className={`hud-view-btn${displayState.projection === "perspective" ? " is-active" : ""}`}
            aria-pressed={displayState.projection === "perspective"}
            title={`切换正交 / 透视投影 (P) · 当前：${displayState.projection === "orthographic" ? "正交" : "透视"}`}
            onClick={onToggleProjection}
          >
            <CadIcon
              name={displayState.projection === "orthographic" ? "orthographic" : "perspective"}
              size={13}
            />
            <span>{displayState.projection === "orthographic" ? "正交" : "透视"}</span>
          </button>

          {/* Visibility Categories & Semantic Filter */}
          <div className="hud-dropdown-anchor">
            <button
              type="button"
              className={`hud-view-btn${openMenu === "visibility" ? " is-active" : ""}`}
              aria-expanded={openMenu === "visibility"}
              aria-haspopup="menu"
              title="对象类别可见性分层 (L1)"
              onClick={() => setOpenMenu((cur) => (cur === "visibility" ? null : "visibility"))}
            >
              <CadIcon name="layers" size={13} />
              <span>显示分层</span>
              <CadIcon name="chevronDown" size={11} />
            </button>

            {openMenu === "visibility" && (
              <div className="hud-popover-menu hud-visibility-menu" role="menu" aria-label="对象类别可见性">
                <div className="hud-popover-header">
                  <span className="hud-popover-section-title">对象类别主控 (L1)</span>
                  <button type="button" className="hud-link-btn" onClick={onResetCategories}>
                    恢复默认
                  </button>
                </div>
                {DISPLAY_CATEGORY_META.map((cat) => {
                  const checked = displayState.categories[cat.key];
                  return (
                    <label key={cat.key} className="hud-check-row" title={cat.description}>
                      <input
                        type="checkbox"
                        checked={checked}
                        onChange={() => onToggleCategory(cat.key)}
                      />
                      <CadIcon name={checked ? "eye" : "eyeOff"} size={13} />
                      <span>{cat.label}</span>
                    </label>
                  );
                })}
                {availableSemanticTags.length > 0 && onSelectSemanticFilter && (
                  <>
                    <div className="hud-popover-divider" role="separator" />
                    <div className="hud-popover-header">
                      <span className="hud-popover-section-title">语义标签过滤 (L2)</span>
                      {displayState.activeSemanticFilter && (
                        <button
                          type="button"
                          className="hud-link-btn"
                          onClick={() => onSelectSemanticFilter(null)}
                        >
                          清除过滤
                        </button>
                      )}
                    </div>
                    <div className="hud-semantic-tags">
                      {availableSemanticTags.slice(0, 10).map((tag) => {
                        const active = displayState.activeSemanticFilter === tag;
                        return (
                          <button
                            key={tag}
                            type="button"
                            className={`hud-preset-chip${active ? " is-active" : ""}`}
                            aria-pressed={active}
                            onClick={() => onSelectSemanticFilter(active ? null : tag)}
                          >
                            {tag}
                          </button>
                        );
                      })}
                    </div>
                  </>
                )}
                {hasOverrides && (
                  <>
                    <div className="hud-popover-divider" role="separator" />
                    <div className="hud-popover-footer">
                      <span className="hud-override-summary">
                        {hiddenCount > 0 ? `隐藏 ${hiddenCount}` : ""}
                        {hiddenCount > 0 && translucentCount > 0 ? " · " : ""}
                        {translucentCount > 0 ? `半透明 ${translucentCount}` : ""}
                        {displayState.isolatedIds ? " · 孤立中" : ""}
                      </span>
                      <button
                        type="button"
                        className="hud-link-btn"
                        onClick={() => {
                          onClearAllOverrides();
                          setOpenMenu(null);
                        }}
                      >
                        清除全部覆写
                      </button>
                    </div>
                  </>
                )}
              </div>
            )}
          </div>

          <div className="hud-bar-divider" aria-hidden="true" />

          {/* Standard Camera Views */}
          <div className="hud-dropdown-anchor">
            <button
              type="button"
              className={`hud-view-btn${openMenu === "camera" ? " is-active" : ""}`}
              aria-expanded={openMenu === "camera"}
              aria-haspopup="menu"
              title="标准视角与相机控制 (Space)"
              onClick={() => setOpenMenu((cur) => (cur === "camera" ? null : "camera"))}
            >
              <CadIcon name="camera" size={13} />
              <span>视角</span>
              <CadIcon name="chevronDown" size={11} />
            </button>

            {openMenu === "camera" && (
              <div className="hud-popover-menu hud-camera-menu" role="menu" aria-label="标准视角与相机控制">
                <div className="hud-popover-section-title">标准视角</div>
                <div className="hud-preset-grid">
                  {VIEW_PRESET_OPTIONS.map((preset) => (
                    <button
                      key={preset.id}
                      type="button"
                      className="hud-preset-chip"
                      title={preset.shortcut ? `${preset.label} (${preset.shortcut})` : preset.label}
                      onClick={() => {
                        onApplyPreset(preset.id);
                        setOpenMenu(null);
                      }}
                    >
                      {preset.shortLabel}
                    </button>
                  ))}
                </div>
                <div className="hud-popover-divider" role="separator" />
                <div className="hud-popover-section-title">视角变换与缩放</div>
                <div className="hud-camera-actions-row">
                  {onRotateStep && (
                    <>
                      <button
                        type="button"
                        className="hud-preset-chip"
                        title="左转 90°"
                        onClick={() => onRotateStep(TURN_LEFT_YAW)}
                      >
                        <CadIcon name="rotateLeft" size={12} />
                        <span>左转90°</span>
                      </button>
                      <button
                        type="button"
                        className="hud-preset-chip"
                        title="右转 90°"
                        onClick={() => onRotateStep(TURN_RIGHT_YAW)}
                      >
                        <CadIcon name="rotateRight" size={12} />
                        <span>右转90°</span>
                      </button>
                    </>
                  )}
                  {onZoomBy && (
                    <>
                      <button
                        type="button"
                        className="hud-preset-chip"
                        title="放大视角"
                        onClick={() => onZoomBy(1.25)}
                      >
                        <CadIcon name="zoomIn" size={12} />
                        <span>放大</span>
                      </button>
                      <button
                        type="button"
                        className="hud-preset-chip"
                        title="缩小视角"
                        onClick={() => onZoomBy(0.8)}
                      >
                        <CadIcon name="zoomOut" size={12} />
                        <span>缩小</span>
                      </button>
                    </>
                  )}
                </div>
                <div className="hud-popover-divider" role="separator" />
                <button
                  type="button"
                  role="menuitem"
                  className="hud-popover-item"
                  onClick={() => {
                    onFitView();
                    setOpenMenu(null);
                  }}
                >
                  <CadIcon name="fitView" size={14} />
                  <div className="hud-popover-item-text">
                    <strong>适应窗口 (Home / 双击中键)</strong>
                    <span>居中并缩放模型至完整视口</span>
                  </div>
                </button>
                {onNormalToSelection && (
                  <button
                    type="button"
                    role="menuitem"
                    className="hud-popover-item"
                    disabled={selectedTarget?.kind !== "face"}
                    onClick={() => {
                      onNormalToSelection();
                      setOpenMenu(null);
                    }}
                  >
                    <CadIcon name="normalTo" size={14} />
                    <div className="hud-popover-item-text">
                      <strong>正视于选中面 (F8 / N)</strong>
                      <span>{selectedTarget?.kind === "face" ? "将相机正对当前选中平面" : "请先选中一个平面"}</span>
                    </div>
                  </button>
                )}
              </div>
            )}
          </div>

          <button
            type="button"
            className="hud-view-btn"
            title="适应窗口 (Home / 双击中键)"
            onClick={onFitView}
          >
            <CadIcon name="fitView" size={13} />
            <span>适配</span>
          </button>

          <button
            type="button"
            className={`hud-view-btn${sectionCut.enabled ? " is-active" : ""}`}
            aria-pressed={sectionCut.enabled}
            title="开启 / 关闭实时剖切 (X)"
            onClick={() => onChangeSectionCut({ ...sectionCut, enabled: !sectionCut.enabled })}
          >
            <CadIcon name="sectionCut" size={13} />
            <span>剖切</span>
          </button>

          {isAssembly && (
            <button
              type="button"
              className={`hud-view-btn${explodeFactor > 0 ? " is-active" : ""}`}
              aria-pressed={explodeFactor > 0}
              title="切换装配爆炸图 (Shift+E)"
              onClick={() => onChangeExplodeFactor(explodeFactor > 0 ? 0 : 0.6)}
            >
              <CadIcon name="explode" size={13} />
              <span>爆炸</span>
            </button>
          )}

          {(hiddenCount > 0 || translucentCount > 0) && !displayState.isolatedIds && (
            <>
              <div className="hud-bar-divider" aria-hidden="true" />
              <button
                type="button"
                className="hud-view-btn is-warning"
                title="显示全部已隐藏对象 (Shift+Y)"
                onClick={onShowAllEntities}
              >
                <CadIcon name="eye" size={13} />
                <span>
                  {hiddenCount > 0 ? `显示全部 (${hiddenCount})` : `半透明 (${translucentCount})`}
                </span>
              </button>
            </>
          )}
        </div>
      )}

      {/* Top-Right: Interactive ViewCube */}
      {viewportTab === "3d" && (
        <ViewCube
          yaw={cameraOrientation.yaw}
          pitch={cameraOrientation.pitch}
          projection={displayState.projection}
          onApplyPreset={onApplyPreset}
          onFitView={onFitView}
          onToggleProjection={onToggleProjection}
          onRotateStep={onRotateStep}
          onOrbitBy={onOrbitBy}
        />
      )}

      {/* Isolate Mode Banner */}
      {viewportTab === "3d" && displayState.isolatedIds && (
        <div className="viewport-hud-isolate-banner" role="status">
          <CadIcon name="isolate" size={14} />
          <span>孤立模式 · 仅显示 {displayState.isolatedIds.length} 个对象</span>
          <button
            type="button"
            className="hud-chip-close"
            onClick={onExitIsolate}
            title="退出孤立并恢复原显隐状态 (Esc)"
          >
            退出孤立 Esc
          </button>
        </div>
      )}

      {/* Section / Explode Sliders */}
      {viewportTab === "3d" && (sectionCut.enabled || (isAssembly && explodeFactor > 0)) && (
        <div className="viewport-hud-sliders">
          {sectionCut.enabled && (
            <div className="hud-slider-card" role="group" aria-label="实时剖切控制">
              <CadIcon name="sectionCut" size={14} />
              <span className="hud-slider-title">剖切</span>
              <select
                className="hud-mini-select"
                aria-label="剖切轴向"
                value={sectionCut.axis}
                onChange={(event) => {
                  const val = event.target.value;
                  onChangeSectionCut({
                    ...sectionCut,
                    axis: val === "X" ? "X" : val === "Y" ? "Y" : "Z",
                  });
                }}
              >
                <option value="Z">Z 轴</option>
                <option value="Y">Y 轴</option>
                <option value="X">X 轴</option>
              </select>
              <button
                type="button"
                className={`icon-ghost-btn${sectionCut.flip ? " is-active" : ""}`}
                aria-pressed={Boolean(sectionCut.flip)}
                title="翻转剖切保留侧"
                aria-label="翻转剖切方向"
                onClick={() => onChangeSectionCut({ ...sectionCut, flip: !sectionCut.flip })}
              >
                <CadIcon name="flip" size={13} />
              </button>
              <input
                type="range"
                min={0.05}
                max={0.95}
                step={0.02}
                value={sectionCut.ratio}
                aria-label="剖切深度"
                onChange={(event) =>
                  onChangeSectionCut({ ...sectionCut, ratio: Number(event.target.value) })
                }
              />
              <span className="hud-slider-val tabular-nums">{Math.round(sectionCut.ratio * 100)}%</span>
              <button
                type="button"
                className="icon-ghost-btn"
                aria-label="关闭剖切"
                onClick={() => onChangeSectionCut({ ...sectionCut, enabled: false })}
              >
                <CadIcon name="close" size={12} />
              </button>
            </div>
          )}

          {isAssembly && explodeFactor > 0 && (
            <div className="hud-slider-card" role="group" aria-label="装配爆炸图控制">
              <CadIcon name="explode" size={14} />
              <span className="hud-slider-title">爆炸</span>
              <input
                type="range"
                min={0}
                max={1}
                step={0.05}
                value={explodeFactor}
                aria-label="装配体爆炸视图系数"
                onChange={(event) => onChangeExplodeFactor(Number(event.target.value))}
              />
              <span className="hud-slider-val tabular-nums">{Math.round(explodeFactor * 100)}%</span>
              <button
                type="button"
                className="icon-ghost-btn"
                aria-label="重置爆炸视图"
                onClick={() => onChangeExplodeFactor(0)}
              >
                <CadIcon name="close" size={12} />
              </button>
            </div>
          )}
        </div>
      )}
    </>
  );
}
