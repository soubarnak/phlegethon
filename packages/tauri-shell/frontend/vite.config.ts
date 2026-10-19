import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import { fileURLToPath } from "node:url";
import path from "node:path";

const frontendRoot = path.dirname(fileURLToPath(import.meta.url));
// packages/tauri-shell/frontend → repo root (shell-strings lives outside the Vite root)
const repoRoot = path.resolve(frontendRoot, "../../..");
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],
  // Tauri expects a fixed port and fails if it is taken.
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 1421 } : undefined,
    fs: { allow: [repoRoot] },
    watch: { ignored: ["**/src-tauri/**"] },
  },
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  resolve: {
    alias: {
      "@strings": path.resolve(repoRoot, "packages/shell-strings"),
      "@": path.resolve(frontendRoot, "src"),
    },
  },
  css: {
    preprocessorOptions: {
      scss: {
        loadPaths: [path.resolve(frontendRoot, "node_modules")],
        quietDeps: true,
        silenceDeprecations: ["global-builtin", "import", "color-functions"],
      },
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    // WebKitGTK on Void ships modern JS; keep the output readable in dev builds.
    target: process.env.TAURI_ENV_PLATFORM === "windows" ? "chrome105" : "safari15",
    minify: !process.env.TAURI_ENV_DEBUG,
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
    // Vite 8 (Rolldown) replaces output.manualChunks with codeSplitting groups.
    // Vendor code stays in static imports, so nothing is lazy-loaded.
    rolldownOptions: {
      output: {
        codeSplitting: {
          groups: [
            { name: "react", test: /node_modules\/(react|react-dom|scheduler)\//, priority: 30 },
            { name: "carbon-icons", test: /node_modules\/@carbon\/icons/, priority: 20 },
            { name: "carbon", test: /node_modules\/(@carbon|carbon-)/, priority: 10 },
          ],
        },
      },
    },
  },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test/setup.ts"],
    include: ["src/**/*.test.{ts,tsx}"],
    css: false,
  },
});
