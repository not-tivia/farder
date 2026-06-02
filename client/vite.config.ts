import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { viteStaticCopy } from "vite-plugin-static-copy";
export default defineConfig({
  plugins: [
    react(),
    // @browsermt/bergamot-translator ships a Web Worker + WASM blob inside
    // node_modules. The package's main file constructs the worker via
    // `new Worker(new URL('./worker/translator-worker.js', import.meta.url))`,
    // which doesn't reliably resolve after Vite bundling for third-party
    // packages. We copy the worker triple into `dist/bergamot/` (and Vite
    // serves them at /bergamot/ in dev too); FarderBacking overrides
    // loadWorker() to construct the Worker from `/bergamot/translator-worker.js`
    // so importScripts + WASM fetch resolve as siblings.
    viteStaticCopy({
      targets: [
        {
          src: "node_modules/@browsermt/bergamot-translator/worker/*",
          dest: "bergamot",
        },
      ],
    }),
  ],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    // Don't let Vite's file watcher descend into the Rust build output.
    // On Windows, cargo holds locks on files under src-tauri/target while
    // building, and chokidar throws EBUSY trying to watch them. Rust changes
    // are handled by Tauri's own watcher, not Vite, so this is safe.
    watch: { ignored: ["**/src-tauri/**"] },
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: "esnext",
    minify: !process.env.TAURI_DEBUG ? "esbuild" : false,
    sourcemap: !!process.env.TAURI_DEBUG,
  },
});
