import { join } from "node:path";
import { electronApp, is, optimizer } from "@electron-toolkit/utils";
import { AgentRuntime, type Event } from "agent-ts-bindings";
import { app, BrowserWindow, ipcMain, shell } from "electron";
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

	// sendMessage - async
	ipcMain.handle(
		"agent:sendMessage",
		async (_, message: string, modelId: string, providerId: string) => {
			if (!runtime) throw new Error("Runtime not initialized");
			await runtime.sendMessage(message, modelId, providerId);
		},
	);

	// newSession - async
	ipcMain.handle("agent:newSession", async () => {
		if (!runtime) throw new Error("Runtime not initialized");
		await runtime.newSession();
	});

	// getChatHistoryTree - async
	ipcMain.handle("agent:getChatHistoryTree", async () => {
		if (!runtime) throw new Error("Runtime not initialized");
		return await runtime.getChatHistoryTree();
	});

	// approveTool - async
	ipcMain.handle("agent:approveTool", async (_, callId: string) => {
		if (!runtime) throw new Error("Runtime not initialized");
		await runtime.approveTool(callId);
	});

	// denyTool - async
	ipcMain.handle("agent:denyTool", async (_, callId: string) => {
		if (!runtime) throw new Error("Runtime not initialized");
		await runtime.denyTool(callId);
	});

	// executeApprovedTools - async
	ipcMain.handle("agent:executeApprovedTools", async () => {
		if (!runtime) throw new Error("Runtime not initialized");
		await runtime.executeApprovedTools();
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
