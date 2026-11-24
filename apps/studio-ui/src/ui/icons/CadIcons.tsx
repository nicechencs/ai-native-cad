import type { SVGProps } from "react";

export type CadIconName =
  | "logo"
  | "undo"
  | "redo"
  | "search"
  | "command"
  | "sun"
  | "moon"
  | "close"
  | "chevronDown"
  | "chevronRight"
  | "check"
  | "copy"
  | "refresh"
  | "trash"
  | "settings"
  | "bell"
  | "cube"
  | "throughHole"
  | "counterbore"
  | "fillet"
  | "chamfer"
  | "code"
  | "assembly"
  | "assetImport"
  | "export"
  | "measureCircle"
  | "measureFace"
  | "shieldCheck"
  | "boundingBox"
  | "sectionCut"
  | "explode"
  | "view3d"
  | "drawing2d"
  | "viewIso"
  | "viewTop"
  | "viewFront"
  | "viewRight"
  | "sidebarLeft"
  | "sidebarRight"
  | "maximize"
  | "sparkles"
  | "info"
  | "warning"
  | "error"
  | "spinner"
  | "terminal"
  | "properties"
  | "keyboard"
  | "layout"
  | "eye"
  | "eyeOff"
  | "translucent"
  | "wireframe"
  | "shaded"
  | "isolate"
  | "fitView"
  | "perspective"
  | "orthographic"
  | "flip"
  | "layers"
  | "normalTo"
  | "filterAll"
  | "filterFace"
  | "filterEdge"
  | "filterBody"
  | "zoomIn"
  | "zoomOut"
  | "rotateLeft"
  | "rotateRight"
  | "camera"
  | "sketch"
  | "extrude"
  | "revolve"
  | "shell"
  | "draft"
  | "pattern"
  | "mirror"
  | "booleanOp"
  | "directEdit"
  | "datumPlane"
  | "mate"
  | "tag"
  | "pin"
  | "pinOff"
  | "mouse"
  | "fileMenu"
  | "boxPrimitive"
  | "cylinderPrimitive"
  | "plus"
  | "assertGate";

export interface CadIconProps extends Omit<SVGProps<SVGSVGElement>, "name"> {
  readonly name: CadIconName;
  readonly size?: number;
}

export function CadIcon({ name, size = 16, className, ...rest }: CadIconProps) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 20 20"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.5}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      className={`cad-icon${name === "spinner" ? " cad-icon-spin" : ""}${className ? ` ${className}` : ""}`}
      {...rest}
    >
      {renderIconPaths(name)}
    </svg>
  );
}

function renderIconPaths(name: CadIconName) {
  switch (name) {
    case "logo":
      return (
        <>
          <polygon points="10,2.5 16.8,6.4 16.8,13.6 10,17.5 3.2,13.6 3.2,6.4" />
          <path d="M10 10L16.8 6.4M10 10L3.2 6.4M10 10V17.5" opacity="0.6" />
          <circle cx="10" cy="10" r="1.6" fill="currentColor" stroke="none" />
        </>
      );
    case "undo":
      return (
        <>
          <path d="M5.5 7.5H12.5a4 4 0 1 1 0 8H9" />
          <path d="M8 5L5 7.5L8 10" />
        </>
      );
    case "redo":
      return (
        <>
          <path d="M14.5 7.5H7.5a4 4 0 1 0 0 8H11" />
          <path d="M12 5L15 7.5L12 10" />
        </>
      );
    case "search":
      return (
        <>
          <circle cx="9" cy="9" r="5.2" />
          <path d="M13 13L16.8 16.8" />
        </>
      );
    case "command":
      return (
        <>
          <path d="M7.5 7.5H12.5V12.5H7.5Z" />
          <path d="M7.5 7.5V5.5a2 2 0 1 0-2 2H7.5ZM12.5 7.5V5.5a2 2 0 1 1 2 2H12.5ZM7.5 12.5V14.5a2 2 0 1 1-2-2H7.5ZM12.5 12.5V14.5a2 2 0 1 0 2-2H12.5Z" />
        </>
      );
    case "sun":
      return (
        <>
          <circle cx="10" cy="10" r="3.4" />
          <path d="M10 2.5V4.2M10 15.8V17.5M2.5 10H4.2M15.8 10H17.5M4.7 4.7L5.9 5.9M14.1 14.1L15.3 15.3M15.3 4.7L14.1 5.9M5.9 14.1L4.7 15.3" />
        </>
      );
    case "moon":
      return <path d="M15.8 12.2A6.2 6.2 0 0 1 7.8 4.2a6.4 6.4 0 1 0 8 8Z" />;
    case "close":
      return <path d="M5.5 5.5L14.5 14.5M14.5 5.5L5.5 14.5" />;
    case "chevronDown":
      return <path d="M5.5 7.8L10 12.2L14.5 7.8" />;
    case "chevronRight":
      return <path d="M7.8 5.5L12.2 10L7.8 14.5" />;
    case "check":
      return <path d="M4.5 10.5L8.2 14.2L15.8 6.2" />;
    case "copy":
      return (
        <>
          <rect x="7.5" y="7.5" width="8.5" height="8.5" rx="1.5" />
          <path d="M5.5 12.5H4.8A1.3 1.3 0 0 1 3.5 11.2V4.8A1.3 1.3 0 0 1 4.8 3.5H11.2A1.3 1.3 0 0 1 12.5 4.8V5.5" />
        </>
      );
    case "refresh":
      return (
        <>
          <path d="M15.5 10a5.5 5.5 0 1 1-1.4-3.7" />
          <path d="M15.5 4.5V7.5H12.5" />
        </>
      );
    case "trash":
      return (
        <>
          <path d="M4 5.5H16M7.5 5.5V4a1 1 0 0 1 1-1h3a1 1 0 0 1 1 1v1.5" />
          <path d="M5.5 5.5L6.3 16a1.4 1.4 0 0 0 1.4 1.3h4.6a1.4 1.4 0 0 0 1.4-1.3l.8-10.5" />
        </>
      );
    case "settings":
      return (
        <>
          <circle cx="10" cy="10" r="2.6" />
          <path d="M10 3V4.8M10 15.2V17M3 10H4.8M15.2 10H17M5 5L6.3 6.3M13.7 13.7L15 15M15 5L13.7 6.3M6.3 13.7L5 15" />
        </>
      );
    case "bell":
      return (
        <>
          <path d="M6 8.5a4 4 0 0 1 8 0c0 3.2 1.4 4.5 2 5H4c.6-.5 2-1.8 2-5Z" />
          <path d="M8.5 15.8a1.8 1.8 0 0 0 3 0" />
        </>
      );
    case "cube":
      return (
        <>
          <polygon points="10,3 16.5,6.6 16.5,13.4 10,17 3.5,13.4 3.5,6.6" />
          <path d="M10 10L16.5 6.6M10 10L3.5 6.6M10 10V17" />
        </>
      );
    case "throughHole":
      return (
        <>
          <rect x="3" y="4.5" width="14" height="11" rx="1.5" />
          <path d="M7.5 4.5V15.5M12.5 4.5V15.5" />
          <path d="M10 2.5V17.5" strokeDasharray="2 2" opacity="0.55" />
        </>
      );
    case "counterbore":
      return (
        <>
          <rect x="3" y="4.5" width="14" height="11" rx="1.5" />
          <path d="M6 4.5V9H8V15.5M14 4.5V9H12V15.5" />
          <path d="M10 2.5V17.5" strokeDasharray="2 2" opacity="0.55" />
        </>
      );
    case "fillet":
      return (
        <>
          <path d="M4 15.5V9.5A5.5 5.5 0 0 1 9.5 4H15.5" />
          <path d="M4 9.5V4H9.5" strokeDasharray="2 2" opacity="0.45" />
          <circle cx="9.5" cy="9.5" r="1.3" fill="currentColor" stroke="none" />
        </>
      );
    case "chamfer":
      return (
        <>
          <path d="M4 15.5V9.5L9.5 4H15.5" />
          <path d="M4 9.5V4H9.5" strokeDasharray="2 2" opacity="0.45" />
        </>
      );
    case "code":
      return (
        <>
          <path d="M7 6.5L3.5 10L7 13.5" />
          <path d="M13 6.5L16.5 10L13 13.5" />
          <path d="M11.2 4.8L8.8 15.2" opacity="0.7" />
        </>
      );
    case "assembly":
      return (
        <>
          <polygon points="7,3.5 11.5,6 11.5,11 7,13.5 2.5,11 2.5,6" />
          <polygon points="13,7 17.5,9.5 17.5,14.5 13,17 8.5,14.5 8.5,9.5" />
        </>
      );
    case "assetImport":
      return (
        <>
          <path d="M10 3.5V12.5M6.8 9.3L10 12.5L13.2 9.3" />
          <path d="M3.5 13.5V15.5a1.5 1.5 0 0 0 1.5 1.5h10a1.5 1.5 0 0 0 1.5-1.5v-2" />
        </>
      );
    case "export":
      return (
        <>
          <path d="M10 12.5V3.5M6.8 6.7L10 3.5L13.2 6.7" />
          <path d="M3.5 13.5V15.5a1.5 1.5 0 0 0 1.5 1.5h10a1.5 1.5 0 0 0 1.5-1.5v-2" />
        </>
      );
    case "measureCircle":
      return (
        <>
          <circle cx="5.5" cy="10" r="2.8" />
          <circle cx="14.5" cy="10" r="2.8" />
          <path d="M5.5 10H14.5" strokeDasharray="1.8 1.8" />
          <circle cx="5.5" cy="10" r="0.9" fill="currentColor" stroke="none" />
          <circle cx="14.5" cy="10" r="0.9" fill="currentColor" stroke="none" />
        </>
      );
    case "measureFace":
      return (
        <>
          <path d="M4.5 4V16M15.5 4V16" />
          <path d="M6.5 10H13.5M8.2 8.2L6.2 10L8.2 11.8M11.8 8.2L13.8 10L11.8 11.8" />
        </>
      );
    case "shieldCheck":
      return (
        <>
          <path d="M10 2.8L16 5.2V9.8c0 3.5-2.5 6.2-6 7.4-3.5-1.2-6-3.9-6-7.4V5.2L10 2.8Z" />
          <path d="M7.3 10L9.2 11.9L13 8" />
        </>
      );
    case "boundingBox":
      return (
        <>
          <rect x="3.5" y="3.5" width="13" height="13" rx="1" strokeDasharray="2.4 2" />
          <rect x="7" y="7" width="6" height="6" rx="0.8" />
        </>
      );
    case "sectionCut":
      return (
        <>
          <path d="M4 5.5H16V14.5H4Z" opacity="0.45" />
          <path d="M2.5 10H17.5" strokeDasharray="2.5 1.8" />
          <path d="M5.5 10L10 14.5M10 10L14.5 14.5" />
        </>
      );
    case "explode":
      return (
        <>
          <rect x="7.5" y="7.5" width="5" height="5" rx="0.8" />
          <path d="M3.5 3.5L6.5 6.5M16.5 3.5L13.5 6.5M3.5 16.5L6.5 13.5M16.5 16.5L13.5 13.5" />
        </>
      );
    case "view3d":
      return (
        <>
          <polygon points="10,2.8 16.5,6.4 16.5,13.6 10,17.2 3.5,13.6 3.5,6.4" />
          <path d="M10 10L16.5 6.4M10 10L3.5 6.4M10 10V17.2" opacity="0.65" />
        </>
      );
    case "drawing2d":
      return (
        <>
          <rect x="3" y="3.5" width="14" height="13" rx="1.2" />
          <path d="M6 7.5H14M6 11H11M11 13.5H14" />
        </>
      );
    case "viewIso":
      return (
        <>
          <polygon points="10,3.2 16,6.6 16,13.4 10,16.8 4,13.4 4,6.6" />
          <path d="M10 10V16.8M10 10L16 6.6M10 10L4 6.6" />
        </>
      );
    case "viewTop":
      return (
        <>
          <rect x="4" y="4" width="12" height="12" rx="1.2" />
          <rect x="7" y="7" width="6" height="6" rx="0.6" opacity="0.55" />
        </>
      );
    case "viewFront":
      return (
        <>
          <rect x="4" y="5" width="12" height="10" rx="1.2" />
          <circle cx="10" cy="10" r="2.2" />
        </>
      );
    case "viewRight":
      return (
        <>
          <rect x="5" y="4" width="10" height="12" rx="1.2" />
          <path d="M10 4V16" opacity="0.55" />
        </>
      );
    case "sidebarLeft":
      return (
        <>
          <rect x="3" y="3.5" width="14" height="13" rx="1.5" />
          <path d="M7.5 3.5V16.5" />
        </>
      );
    case "sidebarRight":
      return (
        <>
          <rect x="3" y="3.5" width="14" height="13" rx="1.5" />
          <path d="M12.5 3.5V16.5" />
        </>
      );
    case "maximize":
      return (
        <path d="M4 8V4H8M12 4H16V8M16 12V16H12M8 16H4V12" />
      );
    case "sparkles":
      return (
        <>
          <path d="M10 2.8L11.6 7.4L16.2 9L11.6 10.6L10 15.2L8.4 10.6L3.8 9L8.4 7.4L10 2.8Z" />
          <path d="M15.5 13.5L16.2 15.3L18 16L16.2 16.7L15.5 18.5L14.8 16.7L13 16L14.8 15.3L15.5 13.5Z" opacity="0.7" />
        </>
      );
    case "info":
      return (
        <>
          <circle cx="10" cy="10" r="7" />
          <path d="M10 9V13.8" />
          <circle cx="10" cy="6.6" r="0.8" fill="currentColor" stroke="none" />
        </>
      );
    case "warning":
      return (
        <>
          <path d="M10 3.2L17 15.8H3L10 3.2Z" />
          <path d="M10 8V11.8" />
          <circle cx="10" cy="13.8" r="0.8" fill="currentColor" stroke="none" />
        </>
      );
    case "error":
      return (
        <>
          <circle cx="10" cy="10" r="7" />
          <path d="M7.5 7.5L12.5 12.5M12.5 7.5L7.5 12.5" />
        </>
      );
    case "spinner":
      return (
        <>
          <circle cx="10" cy="10" r="6.5" opacity="0.22" />
          <path d="M10 3.5A6.5 6.5 0 0 1 16.5 10" />
        </>
      );
    case "terminal":
      return (
        <>
          <path d="M4.5 6.5L8.5 10L4.5 13.5" />
          <path d="M10.5 13.5H15.5" />
        </>
      );
    case "properties":
      return (
        <>
          <path d="M4 6H16M4 10H16M4 14H16" />
          <circle cx="8" cy="6" r="1.4" fill="currentColor" />
          <circle cx="13" cy="10" r="1.4" fill="currentColor" />
          <circle cx="9.5" cy="14" r="1.4" fill="currentColor" />
        </>
      );
    case "keyboard":
      return (
        <>
          <rect x="2.5" y="5" width="15" height="10" rx="1.8" />
          <path d="M5.5 8.5H6.8M9.3 8.5H10.7M13.2 8.5H14.5M6.5 12H13.5" />
        </>
      );
    case "layout":
      return (
        <>
          <rect x="3" y="3.5" width="14" height="13" rx="1.5" />
          <path d="M7.5 3.5V16.5M12.5 3.5V16.5" />
        </>
      );
    case "eye":
      return (
        <>
          <path d="M2.5 10s2.8-4.8 7.5-4.8S17.5 10 17.5 10s-2.8 4.8-7.5 4.8S2.5 10 2.5 10Z" />
          <circle cx="10" cy="10" r="2.2" />
        </>
      );
    case "eyeOff":
      return (
        <>
          <path d="M3 3l14 14" />
          <path d="M8.5 5.4A7.6 7.6 0 0 1 10 5.2c4.7 0 7.5 4.8 7.5 4.8a13 13 0 0 1-2.5 3" />
          <path d="M5.5 7.2A12.8 12.8 0 0 0 2.5 10s2.8 4.8 7.5 4.8c1 0 1.9-.2 2.7-.5" />
        </>
      );
    case "translucent":
      return (
        <>
          <polygon points="10,3 16.5,6.6 16.5,13.4 10,17 3.5,13.4 3.5,6.6" strokeDasharray="2.2 1.6" />
          <polygon points="10,6 13.5,8 13.5,12 10,14 6.5,12 6.5,8" fill="currentColor" opacity="0.25" />
        </>
      );
    case "wireframe":
      return (
        <>
          <polygon points="10,3 16.5,6.6 16.5,13.4 10,17 3.5,13.4 3.5,6.6" />
          <path d="M10 3V17M3.5 6.6L16.5 13.4M16.5 6.6L3.5 13.4" opacity="0.6" />
        </>
      );
    case "shaded":
      return (
        <>
          <polygon points="10,3 16.5,6.6 16.5,13.4 10,17 3.5,13.4 3.5,6.6" fill="currentColor" opacity="0.22" />
          <polygon points="10,3 16.5,6.6 10,10 3.5,6.6" fill="currentColor" opacity="0.35" stroke="none" />
        </>
      );
    case "isolate":
      return (
        <>
          <circle cx="10" cy="10" r="6.8" strokeDasharray="2.4 2" opacity="0.65" />
          <polygon points="10,6.2 13.4,8.2 13.4,11.8 10,13.8 6.6,11.8 6.6,8.2" />
        </>
      );
    case "fitView":
      return (
        <>
          <path d="M3.5 7V3.5H7M13 3.5H16.5V7M16.5 13V16.5H13M7 16.5H3.5V13" />
          <rect x="7" y="7" width="6" height="6" rx="0.8" />
        </>
      );
    case "perspective":
      return (
        <>
          <polygon points="4,5 16,3.2 16,16.8 4,15" />
          <path d="M4 5L9.5 7.5V12.5L4 15M9.5 7.5L16 3.2M9.5 12.5L16 16.8" opacity="0.6" />
        </>
      );
    case "orthographic":
      return (
        <>
          <rect x="4" y="5.5" width="9" height="9" />
          <path d="M7 3.5H16V12.5" opacity="0.6" />
          <path d="M4 5.5L7 3.5M13 5.5L16 3.5M13 14.5L16 12.5" opacity="0.6" />
        </>
      );
    case "flip":
      return (
        <>
          <path d="M6 7H14M11.5 4.5L14 7L11.5 9.5" />
          <path d="M14 13H6M8.5 10.5L6 13L8.5 15.5" />
        </>
      );
    case "layers":
      return (
        <>
          <polygon points="10,3.5 17,7.2 10,10.9 3,7.2" />
          <path d="M3 10.5L10 14.2L17 10.5" />
          <path d="M3 13.6L10 17.3L17 13.6" />
        </>
      );
    case "normalTo":
      return (
        <>
          <rect x="4.5" y="4.5" width="11" height="11" rx="1.2" />
          <circle cx="10" cy="10" r="2.2" />
          <path d="M10 2V4.5M10 15.5V18M2 10H4.5M15.5 10H18" />
        </>
      );
    case "filterAll":
      return (
        <>
          <path d="M3.5 4.5H16.5L11.5 10.2V15.5L8.5 14V10.2L3.5 4.5Z" />
        </>
      );
    case "filterFace":
      return (
        <>
          <polygon points="10,3 16.5,6.8 16.5,13.2 10,17 3.5,13.2 3.5,6.8" opacity="0.35" />
          <polygon points="10,3 16.5,6.8 10,10.2 3.5,6.8" fill="currentColor" opacity="0.45" />
          <polygon points="10,3 16.5,6.8 10,10.2 3.5,6.8" />
        </>
      );
    case "filterEdge":
      return (
        <>
          <polygon points="10,3 16.5,6.8 16.5,13.2 10,17 3.5,13.2 3.5,6.8" opacity="0.3" />
          <path d="M10 3L16.5 6.8M10 10.2V17" strokeWidth={2.2} />
          <circle cx="10" cy="3" r="1.2" fill="currentColor" stroke="none" />
          <circle cx="16.5" cy="6.8" r="1.2" fill="currentColor" stroke="none" />
        </>
      );
    case "filterBody":
      return (
        <>
          <polygon points="10,3 16.5,6.6 16.5,13.4 10,17 3.5,13.4 3.5,6.6" fill="currentColor" opacity="0.22" />
          <polygon points="10,3 16.5,6.6 16.5,13.4 10,17 3.5,13.4 3.5,6.6" />
          <path d="M10 10L16.5 6.6M10 10L3.5 6.6M10 10V17" />
        </>
      );
    case "zoomIn":
      return (
        <>
          <circle cx="9" cy="9" r="5.2" />
          <path d="M13 13L16.8 16.8M9 6.8V11.2M6.8 9H11.2" />
        </>
      );
    case "zoomOut":
      return (
        <>
          <circle cx="9" cy="9" r="5.2" />
          <path d="M13 13L16.8 16.8M6.8 9H11.2" />
        </>
      );
    case "rotateLeft":
      return (
        <>
          <path d="M5.5 7.5A5.5 5.5 0 1 1 6 14.2" />
          <path d="M5 4.2V7.8H8.6" />
        </>
      );
    case "rotateRight":
      return (
        <>
          <path d="M14.5 7.5A5.5 5.5 0 1 0 14 14.2" />
          <path d="M15 4.2V7.8H11.4" />
        </>
      );
    case "camera":
      return (
        <>
          <rect x="2.8" y="5.5" width="14.4" height="10" rx="1.6" />
          <path d="M7.2 5.5L8.4 3.6H11.6L12.8 5.5" />
          <circle cx="10" cy="10.5" r="2.6" />
        </>
      );
    case "sketch":
      return (
        <>
          <rect x="3.5" y="3.5" width="13" height="13" rx="1.2" strokeDasharray="2.2 1.8" opacity="0.55" />
          <path d="M6 14L13.8 6.2L15.2 7.6L7.4 15.4L5.2 15.8L6 14Z" />
          <circle cx="6.5" cy="6.5" r="1.4" />
        </>
      );
    case "extrude":
      return (
        <>
          <rect x="4" y="10.5" width="12" height="6" rx="1" />
          <path d="M4 10.5L6.5 7.5H13.5L16 10.5" opacity="0.6" />
          <path d="M10 8.5V2.8M7.5 5.2L10 2.8L12.5 5.2" />
        </>
      );
    case "revolve":
      return (
        <>
          <path d="M10 2.8V17.2" strokeDasharray="2 2" opacity="0.6" />
          <path d="M5 7.5A5.5 2.6 0 1 0 15 7.5" />
          <path d="M13 5.4L15.2 7.5L12.6 8.8" />
          <path d="M10 6.5H15V13.5H10" opacity="0.7" />
        </>
      );
    case "shell":
      return (
        <>
          <rect x="3.5" y="4" width="13" height="12" rx="1.5" />
          <rect x="6.5" y="4" width="7" height="9" rx="0.8" />
          <path d="M3.5 7H6.5M13.5 7H16.5" opacity="0.5" />
        </>
      );
    case "draft":
      return (
        <>
          <polygon points="5.5,4.5 14.5,4.5 16.5,15.5 3.5,15.5" />
          <path d="M5.5 4.5V15.5" strokeDasharray="2 2" opacity="0.5" />
          <path d="M14.5 4.5V15.5" strokeDasharray="2 2" opacity="0.5" />
        </>
      );
    case "pattern":
      return (
        <>
          <rect x="3.5" y="3.5" width="5" height="5" rx="0.9" />
          <rect x="11.5" y="3.5" width="5" height="5" rx="0.9" />
          <rect x="3.5" y="11.5" width="5" height="5" rx="0.9" />
          <rect x="11.5" y="11.5" width="5" height="5" rx="0.9" strokeDasharray="1.8 1.4" />
        </>
      );
    case "mirror":
      return (
        <>
          <path d="M10 2.8V17.2" strokeDasharray="2.2 1.8" />
          <polygon points="3.5,6 8,10 3.5,14" />
          <polygon points="16.5,6 12,10 16.5,14" opacity="0.65" />
        </>
      );
    case "booleanOp":
      return (
        <>
          <rect x="3.5" y="5.5" width="8.5" height="8.5" rx="1.2" />
          <circle cx="12.5" cy="10.5" r="4.3" />
        </>
      );
    case "directEdit":
      return (
        <>
          <polygon points="4,7 10,4 16,7 16,13 10,16 4,13" opacity="0.55" />
          <path d="M10 10H17.2M15 7.8L17.2 10L15 12.2" />
          <circle cx="10" cy="10" r="1.4" fill="currentColor" stroke="none" />
        </>
      );
    case "datumPlane":
      return (
        <>
          <polygon points="5,4.5 16.5,6.5 15,15.5 3.5,13.5" strokeDasharray="2.4 1.6" />
          <path d="M10 6V14M6.5 9.5L13.5 10.5" opacity="0.6" />
        </>
      );
    case "mate":
      return (
        <>
          <rect x="3" y="6" width="5.5" height="8" rx="1" />
          <rect x="11.5" y="6" width="5.5" height="8" rx="1" />
          <path d="M8.5 10H11.5M9.5 8.5L11 10L9.5 11.5" />
        </>
      );
    case "tag":
      return (
        <>
          <path d="M3.5 10.2V4.5a1 1 0 0 1 1-1h5.7l6.3 6.3a1.2 1.2 0 0 1 0 1.7l-4.5 4.5a1.2 1.2 0 0 1-1.7 0l-6.8-5.8Z" />
          <circle cx="7.2" cy="7.2" r="1.1" fill="currentColor" stroke="none" />
        </>
      );
    case "pin":
      return (
        <>
          <path d="M7.5 3.5H12.5L11.8 8.5L14.2 11H5.8L8.2 8.5L7.5 3.5Z" />
          <path d="M10 11V17" />
        </>
      );
    case "pinOff":
      return (
        <>
          <path d="M3.5 3.5L16.5 16.5" />
          <path d="M8 4H12.5L11.8 8.2L13.8 10.5H11" />
          <path d="M10 12.5V16.8" />
        </>
      );
    case "mouse":
      return (
        <>
          <rect x="5.5" y="3" width="9" height="14" rx="4.5" />
          <path d="M10 3V8.2M5.5 8.2H14.5" />
          <path d="M10 5.2V7.2" strokeWidth={2} />
        </>
      );
    case "fileMenu":
      return (
        <>
          <path d="M5 3.5H11.5L15 7V16.5H5V3.5Z" />
          <path d="M11.5 3.5V7H15" />
        </>
      );
    case "boxPrimitive":
      return (
        <>
          <polygon points="10,3.2 16.2,6.4 16.2,13.6 10,16.8 3.8,13.6 3.8,6.4" />
          <path d="M10 9.6L16.2 6.4M10 9.6L3.8 6.4M10 9.6V16.8" />
        </>
      );
    case "cylinderPrimitive":
      return (
        <>
          <ellipse cx="10" cy="5.5" rx="5.5" ry="2.2" />
          <path d="M4.5 5.5V14.5C4.5 15.7 7 16.7 10 16.7C13 16.7 15.5 15.7 15.5 14.5V5.5" />
        </>
      );
    case "plus":
      return <path d="M10 4.5V15.5M4.5 10H15.5" />;
    case "assertGate":
      return (
        <>
          <rect x="3.5" y="3.5" width="13" height="13" rx="2" />
          <path d="M6.8 10.2L9 12.4L13.5 7.6" />
        </>
      );
  }
}
