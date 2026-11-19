import type { AddressInfo } from "node:net";
import type { EditHandle } from "./src/types/scene.ts";

export interface StudioHttpServer {
  listen(port: number, hostname?: string, listeningListener?: () => void): this;
  address(): AddressInfo | string | null;
  close(callback?: (err?: Error) => void): this;
}

export function createStudioServer(): StudioHttpServer;
export function enrichEditHandles(
  source: string,
  handles: readonly EditHandle[],
): EditHandle[];
