import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

export default defineConfig({
  plugins: [svelte()],
  server: { proxy: { "/api": { target: "http://127.0.0.1:3928", changeOrigin: true } } },
});
