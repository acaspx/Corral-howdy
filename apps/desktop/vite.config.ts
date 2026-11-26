import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

// Tauri expects a fixed dev port and serves the built files from ../dist (see src-tauri/tauri.conf.json).
export default defineConfig({
  plugins: [react()],
  // `@/` is src, as Glaze's components import it (components.json).
  resolve: { alias: { "@": decodeURIComponent(new URL("./src", import.meta.url).pathname) } },
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: { target: "safari18", outDir: "dist" },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test-setup.ts"],
    // The stylesheet as written, for the focus-ring check (src/a11y.test.tsx, TP-A11Y-06).
    css: { include: [/\/src\/styles\.css(\?|$)/] },
  },
});
