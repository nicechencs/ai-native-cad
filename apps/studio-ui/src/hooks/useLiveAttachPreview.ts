/**
 * Opens Change Preview when the desktop host emits `live-attach://preview-ready`.
 * The listener is a no-op in the browser.
 */
import { useEffect } from "react";
import { subscribeLiveAttachPreviewReady } from "../sync/liveAttachListener.ts";
import { useEngineeringSession } from "./useEngineeringSession.ts";

export function useLiveAttachPreviewReady(): void {
  const { dispatchSemanticCommand } = useEngineeringSession();
  useEffect(() => {
    return subscribeLiveAttachPreviewReady((preview) => {
      dispatchSemanticCommand({ op: "open_preview", preview });
    });
  }, [dispatchSemanticCommand]);
}
