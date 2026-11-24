# Studio Workbench Guide

**English** | [简体中文](../zh/workbench-guide.md) | [Docs Index](../README.md)

AI-Native CAD Studio (`apps/studio-ui` + `apps/desktop`) combines industrial CAD ergonomics inspired by **Siemens NX** and **PTC Creo** with inline Point-and-Prompt AI editing and branch-isolated `Change Preview` approvals.

---

## 1. Workspace Layout

- **File Backstage**: New, open, and save `.aicad` projects or `.kcl` files; import external 3D assets (STEP, 3MF, STL, OBJ, glTF); export manufacturing deliverables; configure workbench preferences.
- **Ribbon & Top Border Bar**: Organized into **7 permanent tabs** plus **2 dynamic contextual tabs (Sketch / 2D Drawing)**, paired with a permanent Top Border Bar for selection filters and command search.
- **Left Navigator**: Switch between the **Fault-Tolerant Feature Tree** (with rollback highlighting and node suppression), **Virtual Assembly Tree** (component selection & explode linkage), **Parameters / Formulas**, and the **Monaco `.kcl` Editor** with LSP hover/completions. On narrow viewports (`< 1280px`), left and right docks collapse mutually to preserve viewport space.
- **Center 3D / 2D Viewport**:
  - Dual WebGL2 and zero-copy `CMS2` WebGPU rendering paths;
  - Built-in **ViewCube**, floating **Mini Toolbar**, **QuickPick** overlap disambiguation, **8-direction Radial Menu**, live section clipping planes, and assembly explode slider;
  - Switchable **Associative 2D Drawing** tab.
- **Right Collapsible Tool Rail**: Collapsed by default to a slim vertical icon rail (preserving `>= 78%` canvas width). Expand on demand for the **AI Assistant**, **Feature Property & Explain Inspector**, **L0–L4 Validation Panel**, and **Export Panel**.

---

## 2. Navigation Presets & Shortcuts

The viewport defaults to **NX / Creo mouse navigation** and can be switched anytime to **SolidWorks** or **Web / Trackpad** presets (persisted automatically):

| Action | NX / Creo (Default) | SolidWorks Preset | Web / Trackpad Preset |
| :--- | :--- | :--- | :--- |
| **Rotate** | Hold **Middle Mouse (MMB)** + drag | Hold **MMB** + drag | Left-drag / two-finger drag |
| **Pan** | `Shift + MMB` or `RMB + MMB` drag | `Ctrl + MMB` drag | Right-drag / `Shift + Left-drag` |
| **Zoom** | Scroll wheel (cursor-centered) | Scroll wheel | Scroll wheel / pinch |
| **Fit View** | Double-click **MMB** or `Ctrl + F` | `F` / double-click MMB | Double-click background / `Ctrl + F` |
| **Pick / Box Select** | Left-click (LMB) & Window/Crossing box | Left-click | Left-click |
| **Context / Radial Menu** | Short right-click context menu; hold & swipe for Radial Menu | Right-click menu | Right-click menu |

**Four ways to switch presets**:
1. Status-bar navigation dropdown;
2. `File` Backstage preferences page;
3. Ribbon `Tools & Code` tab;
4. Command line: `nav nx`, `nav sw`, or `nav web`.

**Key Shortcuts**:
- `Ctrl + K`: Open Command Palette
- `Alt + A`: Open inline **Point-and-Prompt** popover near selected geometry
- `Ctrl + Shift + A`: Toggle full right-side AI Assistant panel
- `Ctrl + Z` / `Ctrl + Y`: Transactional Undo / Redo (backed by `.aicad` SQLite `oplog`)
- `Ctrl + S`: Save current project or download source

---

## 3. Feature Creation (Feature Dashboard)

Invoking primitives, holes, fillets, chamfers, shells, or patterns opens the horizontal Creo-style **Feature Dashboard** above the viewport:
- Displays active geometry collectors (`TopoRef` selections) alongside parameter fields;
- Supports the NX-style **Apply** (continuous creation), **OK**, and **Cancel** workflow;
- Commits structured `SemanticCommand` transactions through the unified runtime so the feature tree, `.kcl` view, and validation report stay synchronized.

---

## 4. AI Assistance & Change Preview Workflow

1. **Configure Provider**: Open AI Settings in the right rail to configure an OpenAI-compatible or Gemini API key (stored locally in profile storage).
2. **Point-and-Prompt**: Select one or more faces/edges in the 3D viewport and press `Alt + A` (or use the Mini Toolbar / Radial Menu). Describe the local edit (e.g., *"Change the selected bore to an M6 counterbore"*). Structured `TopoRef` and `node_id` metadata are forwarded to the backend so only the intended local context slice is edited.
3. **Branch-Isolated Change Preview**:
   - AI edits—whether from the built-in assistant or an external Claude Code / Codex agent attached via Live-Attach—evaluate on an isolated preview branch without mutating `main`;
   - Inspect parameter/feature diffs and L0–L4 tri-state checks in `ViewportChangePreviewBar` and `ChangePreviewCard`;
   - Click **Commit Preview** to promote onto `main`, or **Discard** to leave `main` untouched.

---

## 5. Opening Sample Models & Exporting

- Use `File -> Open` to load `.aicad` projects from `examples/aicad-projects/` or `.kcl` models from `examples/mvp-tier1..3/`;
- Imported open meshes (such as `open_shell.stl`) are restricted to preview/measurement and cannot enter exact booleans;
- Export whole parts or individual assembly components to `STEP AP242`, `3MF`, `DXF`, and `SVG` drawings from the Export panel, which explicitly reports what each format preserves.
