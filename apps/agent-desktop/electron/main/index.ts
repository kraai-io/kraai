import { join } from "node:path";
import { electronApp, is, optimizer } from "@electron-toolkit/utils";
import { app, BrowserWindow, ipcMain, shell } from "electron";
import icon from "../../resources/icon.png?asset";
import { AgentApi, type FileError } from "agent-ts-bindings";

// Store the AgentApi instance - this is the source of truth
let agentApi: AgentApi | null = null;

// Initialize AgentApi with file operation callbacks
function initializeAgentApi(): AgentApi {
  console.log("[MAIN] Initializing AgentApi...");

  // File read callback - runs in main process, has full filesystem access
  const readFileCallback = (
    _err: Error | null,
    filePath: string,
  ): FileError | Uint8Array => {
    console.log("[MAIN] readFile callback called for:", filePath);
    try {
      const fs = require("node:fs");
      const data = fs.readFileSync(filePath);
      return new Uint8Array(data);
    } catch (e: any) {
      if (e.code === "ENOENT") {
        return { type: "NotFound", path: filePath } as FileError;
      }
      return {
        type: "IoError",
        path: filePath,
        message: e.message,
      } as FileError;
    }
  };

  // File write callback
  const writeFileCallback = (
    _err: Error | null,
    filePath: string,
    data: Uint8Array,
  ): FileError | undefined => {
    console.log("[MAIN] writeFile callback called for:", filePath);
    try {
      const fs = require("node:fs");
      fs.writeFileSync(filePath, Buffer.from(data));
      return undefined;
    } catch (e: any) {
      return {
        type: "IoError",
        path: filePath,
        message: e.message,
      } as FileError;
    }
  };

  // List directory callback
  const listDirCallback = (
    _err: Error | null,
    dirPath: string,
  ): FileError | string[] => {
    console.log("[MAIN] listDir callback called for:", dirPath);
    try {
      const fs = require("node:fs");
      const entries = fs.readdirSync(dirPath, { withFileTypes: true });
      return entries.map((e: any) => e.name);
    } catch (e: any) {
      if (e.code === "ENOENT") {
        return { type: "NotFound", path: dirPath } as FileError;
      }
      return {
        type: "IoError",
        path: dirPath,
        message: e.message,
      } as FileError;
    }
  };

  agentApi = new AgentApi(readFileCallback, writeFileCallback, listDirCallback);
  console.log("[MAIN] AgentApi initialized successfully");

  // Test sandbox escape immediately
  console.log("[MAIN] Testing sandbox escape with testReadRootDir...");
  agentApi
    .testReadRootDir()
    .then((entries) => {
      console.log("[MAIN] ✓ Sandbox escape SUCCESS - Root directory contents:", entries);
    })
    .catch((e) => {
      console.error("[MAIN] ✗ Sandbox escape FAILED:", e);
    });

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

  // readFile - async
  ipcMain.handle("agent:readFile", async (_event, filePath: string) => {
    if (!agentApi) {
      throw new Error("AgentApi not initialized");
    }
    return await agentApi.readFile(filePath);
  });

  // writeFile - async
  ipcMain.handle(
    "agent:writeFile",
    async (_event, filePath: string, data: number[]) => {
      if (!agentApi) {
        throw new Error("AgentApi not initialized");
      }
      return await agentApi.writeFile(filePath, new Uint8Array(data));
    },
  );

  // listDir - async
  ipcMain.handle("agent:listDir", async (_event, dirPath: string) => {
    if (!agentApi) {
      throw new Error("AgentApi not initialized");
    }
    return await agentApi.listDir(dirPath);
  });

  // testReadRootDir - async (sandbox escape test)
  ipcMain.handle("agent:testReadRootDir", async () => {
    if (!agentApi) {
      throw new Error("AgentApi not initialized");
    }
    return await agentApi.testReadRootDir();
  });

  // reloadConfig - async
  ipcMain.handle(
    "agent:reloadConfig",
    async (_event, configData: number[], configDir: string) => {
      if (!agentApi) {
        throw new Error("AgentApi not initialized");
      }
      return await agentApi.reloadConfig(new Uint8Array(configData), configDir);
    },
  );

  console.log("[MAIN] IPC handlers set up successfully");
}

// Config directory handling
function getConfigDir(): string {
  const path = require("node:path");
  const { app } = require("electron");
  return path.join(app.getPath("home"), ".agent-desktop");
}

async function ensureDefaultConfig(): Promise<void> {
  const fs = require("node:fs/promises");
  const path = require("node:path");
  const configDir = getConfigDir();
  const configPath = path.join(configDir, "config.toml");

  try {
    await fs.access(configPath);
  } catch {
    const defaultConfig = `# Default Agent Configuration
# Add your providers and models here

# Example provider:
# [[provider]]
# id = "openai"
# type = "openai"
# base_url = "https://api.openai.com/v1"
# env_var_api_key = "OPENAI_API_KEY"

# Example model:
# [[model]]
# id = "gpt-4"
# provider_id = "openai"
`;
    await fs.mkdir(configDir, { recursive: true });
    await fs.writeFile(configPath, defaultConfig, "utf-8");
  }
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
app.whenReady().then(async () => {
  electronApp.setAppUserModelId("com.ominit.agent");

  app.on("browser-window-created", (_, window) => {
    optimizer.watchWindowShortcuts(window);
  });

  // Initialize AgentApi in main process (source of truth)
  await ensureDefaultConfig();
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
