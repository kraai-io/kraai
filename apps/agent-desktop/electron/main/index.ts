import { join } from "node:path";
import { electronApp, is, optimizer } from "@electron-toolkit/utils";
import { AgentRuntime, type Event } from "agent-ts-bindings";
import {
	app,
	BrowserWindow,
	dialog,
	ipcMain,
	type OpenDialogOptions,
	shell,
} from "electron";
import icon from "../../resources/icon.png?asset";

// Store the runtime and main window
let runtime: AgentRuntime | null = null;
let mainWindow: BrowserWindow | null = null;

function initializeRuntime() {
	console.log("[MAIN] Initializing AgentRuntime...");

	// Create event callback that forwards to renderer
	// NAPI-RS callbacks have signature (err, arg) => void
	const eventCallback = (err: Error | null, event: Event) => {
		if (err) {
			console.error("[MAIN] Event callback error:", err);
			return;
		}
		console.log("[MAIN] Forwarding event to renderer:", event.type);
		if (mainWindow && !mainWindow.isDestroyed()) {
			mainWindow.webContents.send("agent:event", event);
		}
	};

	runtime = new AgentRuntime(eventCallback);
	console.log("[MAIN] AgentRuntime initialized");
}

function setupIpcHandlers() {
	console.log("[MAIN] Setting up IPC handlers...");

	// listModels - async
	ipcMain.handle("agent:listModels", async () => {
		if (!runtime) throw new Error("Runtime not initialized");
		return await runtime.listModels();
	});

	ipcMain.handle("agent:getSettings", async () => {
		if (!runtime) throw new Error("Runtime not initialized");
		return await runtime.getSettings();
	});

	ipcMain.handle("agent:saveSettings", async (_, settings) => {
		if (!runtime) throw new Error("Runtime not initialized");
		await runtime.saveSettings(settings);
	});

	ipcMain.handle("agent:createSession", async () => {
		if (!runtime) throw new Error("Runtime not initialized");
		return await runtime.createSession();
	});

	// sendMessage - async
	ipcMain.handle(
		"agent:sendMessage",
		async (
			_,
			sessionId: string,
			message: string,
			modelId: string,
			providerId: string,
		) => {
			if (!runtime) throw new Error("Runtime not initialized");
			await runtime.sendMessage(sessionId, message, modelId, providerId);
		},
	);

	// getChatHistoryTree - async
	ipcMain.handle("agent:getChatHistoryTree", async (_, sessionId: string) => {
		if (!runtime) throw new Error("Runtime not initialized");
		return await runtime.getChatHistoryTree(sessionId);
	});

	// approveTool - async
	ipcMain.handle(
		"agent:approveTool",
		async (_, sessionId: string, callId: string) => {
			if (!runtime) throw new Error("Runtime not initialized");
			await runtime.approveTool(sessionId, callId);
		},
	);

	// denyTool - async
	ipcMain.handle(
		"agent:denyTool",
		async (_, sessionId: string, callId: string) => {
			if (!runtime) throw new Error("Runtime not initialized");
			await runtime.denyTool(sessionId, callId);
		},
	);

	ipcMain.handle("agent:cancelStream", async (_, sessionId: string) => {
		if (!runtime) throw new Error("Runtime not initialized");
		return await runtime.cancelStream(sessionId);
	});

	// executeApprovedTools - async
	ipcMain.handle("agent:executeApprovedTools", async (_, sessionId: string) => {
		if (!runtime) throw new Error("Runtime not initialized");
		await runtime.executeApprovedTools(sessionId);
	});

	// listSessions - async
	ipcMain.handle("agent:listSessions", async () => {
		if (!runtime) throw new Error("Runtime not initialized");
		return await runtime.listSessions();
	});

	// loadSession - async
	ipcMain.handle("agent:loadSession", async (_, sessionId: string) => {
		if (!runtime) throw new Error("Runtime not initialized");
		return await runtime.loadSession(sessionId);
	});

	// deleteSession - async
	ipcMain.handle("agent:deleteSession", async (_, sessionId: string) => {
		if (!runtime) throw new Error("Runtime not initialized");
		await runtime.deleteSession(sessionId);
	});

	ipcMain.handle("agent:getWorkspaceState", async (_, sessionId: string) => {
		if (!runtime) throw new Error("Runtime not initialized");
		return await runtime.getWorkspaceState(sessionId);
	});

	ipcMain.handle(
		"agent:setWorkspaceDir",
		async (_, sessionId: string, workspaceDir: string) => {
			if (!runtime) throw new Error("Runtime not initialized");
			await runtime.setWorkspaceDir(sessionId, workspaceDir);
		},
	);

	ipcMain.handle("agent:pickWorkspaceDir", async (_, defaultPath?: string) => {
		const owner =
			mainWindow && !mainWindow.isDestroyed() ? mainWindow : undefined;
		const options: OpenDialogOptions = {
			properties: ["openDirectory"],
			defaultPath,
		};
		const result = owner
			? await dialog.showOpenDialog(owner, options)
			: await dialog.showOpenDialog(options);
		if (result.canceled || result.filePaths.length === 0) {
			return null;
		}
		return result.filePaths[0];
	});

	console.log("[MAIN] IPC handlers set up");
}

function createWindow(): void {
	mainWindow = new BrowserWindow({
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
		mainWindow?.show();
	});

	mainWindow.on("closed", () => {
		mainWindow = null;
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

app.whenReady().then(() => {
	electronApp.setAppUserModelId("com.ominit.agent");

	app.on("browser-window-created", (_, window) => {
		optimizer.watchWindowShortcuts(window);
	});

	initializeRuntime();
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
