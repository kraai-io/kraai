import { defineConfig } from "vite";

export default defineConfig({
  // Relative asset URLs, so the built page works from any path or host.
  base: "./",
  // The page imports logos straight from the repository's assets/ folder.
  server: { fs: { allow: ["../.."] } },
});
