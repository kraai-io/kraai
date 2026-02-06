import { electronAPI } from "@electron-toolkit/preload";
import { contextBridge, ipcRenderer } from "electron";

// API that proxies calls to the main process AgentApi
// The actual AgentApi instance lives in the main process
const agentApiProxy = {
  // Synchronous method
  listModels(): any[] {
    return ipcRenderer.sendSync("agent:listModels");
  },
};

// Custom APIs for renderer
const api = {
  agentApi: agentApiProxy,
};

// Use contextBridge APIs to expose Electron APIs to renderer
if (process.contextIsolated) {
  try {
    contextBridge.exposeInMainWorld("electron", electronAPI);
    contextBridge.exposeInMainWorld("api", api);
    console.log("[PRELOAD] APIs exposed via contextBridge");
  } catch (error) {
    console.error("[PRELOAD] Failed to expose APIs:", error);
  }
} else {
  // @ts-expect-error (define in dts)
  window.electron = electronAPI;
  // @ts-expect-error (define in dts)
  window.api = api;
  console.log("[PRELOAD] APIs exposed via window object");
}

console.log("[PRELOAD] Preload script loaded successfully");
