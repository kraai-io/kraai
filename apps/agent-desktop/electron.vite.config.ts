import react from "@vitejs/plugin-react";
import { defineConfig } from "electron-vite";
import tailwindcss from "@tailwindcss/vite"
import { resolve } from "path";

export default defineConfig({
	main: {
    build: {
      rollupOptions: {
        input: {
          index: resolve(__dirname, 'electron/main/index.ts')
        }
      }
    }
  },
  preload: {
    build: {
      rollupOptions: {
        input: {
          index: resolve(__dirname, 'electron/preload/index.ts')
        }
      }
    }
  },
	renderer: {
	  resolve: {
           alias: {
                   "@renderer": resolve("src"),
                   "@": resolve(".")
           },
   },
	  root: '.',
    build: {
      rollupOptions: {
        input: {
          index: resolve(__dirname, 'index.html')
        }
      }
    },
		plugins: [react(), tailwindcss()],
	},
});
