import { join } from "node:path";
import { electronApp, is, optimizer } from "@electron-toolkit/utils";
import { app, BrowserWindow, ipcMain, shell } from "electron";
import icon from "../../resources/icon.png?asset";
import { AgentApi } from "agent-ts-bindings";

// Store the AgentApi instance - this is the source of truth
let agentApi: AgentApi | null = null;

// Initialize AgentApi
function initializeAgentApi(): AgentApi {
  console.log("[MAIN] Initializing AgentApi...");

  agentApi = new AgentApi();
  console.log("[MAIN] AgentApi initialized successfully");

  return agentApi;
}

// Set up IPC handlers for renderer to call AgentApi methods
function setupIpcHandlers() {
  console.log("[MAIN] Setting up IPC handlers...");

  // listModels - synchronous (using on + returnValue for sendSync)
  ipcMain.on("agent:listModels", (event) => {
    if (!agentApi) {
      event.returnValue = [];
      return;
    }
    try {
      event.returnValue = agentApi.listModels();
    } catch (e) {
      console.error("[MAIN] listModels error:", e);
      event.returnValue = [];
    }
  });

  console.log("[MAIN] IPC handlers set up successfully");
}

function createWindow(): void {
  const mainWindow = new BrowserWindow({
    width: 900,
    height: 670,
    show: false,
    autoHideMenuBar: true,
    ...(process.platform === "linux" ? { icon } : {}),
    webPreferences: {
      preload: join(__dirname, "../preload/index.js"),
      sandbox: false,
    },
  });

  mainWindow.on("ready-to-show", () => {
    mainWindow.show();
  });

  mainWindow.webContents.setWindowOpenHandler((details) => {
    shell.openExternal(details.url);
    return { action: "deny" };
  });

  if (is.dev && process.env.ELECTRON_RENDERER_URL) {
    mainWindow.loadURL(process.env.ELECTRON_RENDERER_URL);
  } else {
    mainWindow.loadFile(join(__dirname, "../renderer/index.html"));
  }
}

// App lifecycle
app.whenReady().then(() => {
  electronApp.setAppUserModelId("com.ominit.agent");

  app.on("browser-window-created", (_, window) => {
    optimizer.watchWindowShortcuts(window);
  });

  // Initialize AgentApi in main process (source of truth)
  initializeAgentApi();
  setupIpcHandlers();

  createWindow();

  app.on("activate", () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow();
  });
});

app.on("window-all-closed", () => {
  if (process.platform !== "darwin") {
    app.quit();
  }
});
