import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  optimizeDeps: {
    // The STEP reader loads in a lazy worker. Discover it before the first import
    // so dependency optimization cannot reload the page and clear its in-memory file.
    include: ["occt-import-js"],
  },
  server: {
    port: 5173,
    strictPort: true,
    // Optional same-origin access to `pnpm dev:server` started on a separate port.
    // http-proxy only listens for IncomingMessage "aborted", which Node does not
    // emit once the request body has already been read. An export cancel happens
    // in that window, so close the upstream socket when the browser disconnects.
    proxy: process.env.CAD_STUDIO_API_PROXY
      ? {
          "/api": {
            target: process.env.CAD_STUDIO_API_PROXY,
            configure(proxy) {
              proxy.on("proxyReq", (proxyReq, _req, res) => {
                res.on("close", () => {
                  if (!res.writableEnded) proxyReq.destroy();
                });
              });
            },
          },
        }
      : undefined,
  },
  build: {
    target: "es2022",
    outDir: "dist",
    sourcemap: true,
  },
});
