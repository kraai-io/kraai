import { electronAPI } from "@electron-toolkit/preload";
import { contextBridge, ipcRenderer } from "electron";

// API that proxies calls to the main process AgentApi
// The actual AgentApi instance lives in the main process and has full system access
const agentApiProxy = {
  // Synchronous method
  listModels(): any[] {
    return ipcRenderer.sendSync("agent:listModels");
  },

  // Async methods
  async readFile(path: string): Promise<any> {
    return await ipcRenderer.invoke("agent:readFile", path);
  },

  async writeFile(path: string, data: Uint8Array): Promise<any> {
    return await ipcRenderer.invoke("agent:writeFile", path, Array.from(data));
  },

  async listDir(path: string): Promise<any> {
    return await ipcRenderer.invoke("agent:listDir", path);
  },

  async testReadRootDir(): Promise<string[]> {
    return await ipcRenderer.invoke("agent:testReadRootDir");
  },

  async reloadConfig(configData: Uint8Array, configDir: string): Promise<void> {
    return await ipcRenderer.invoke(
      "agent:reloadConfig",
      Array.from(configData),
      configDir,
    );
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
