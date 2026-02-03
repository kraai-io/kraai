import { join } from "node:path";
import { electronApp, is, optimizer } from "@electron-toolkit/utils";
import { app, BrowserWindow, ipcMain, shell } from "electron";
import icon from "../../resources/icon.png?asset";
import { ConfigWatcher } from "./config-watcher";
import { FileHandler } from "./file-handler";

// Initialize file handler
const fileHandler = new FileHandler();
fileHandler.ensureDefaultConfig();

// Set up IPC handlers for file operations
ipcMain.on("file:read", (event, filePath: string) => {
	fileHandler.readFile(filePath).then((result) => {
		event.returnValue = result;
	});
});

ipcMain.on("file:write", (event, filePath: string, data: number[]) => {
	const uint8Array = new Uint8Array(data);
	fileHandler.writeFile(filePath, uint8Array).then((result) => {
		event.returnValue = result;
	});
});

ipcMain.on("file:listDir", (event, dirPath: string) => {
	fileHandler.listDir(dirPath).then((result) => {
		event.returnValue = result;
	});
});

// Initialize config watcher
let configWatcher: ConfigWatcher | null = null;

function createWindow(): void {
	// Create the browser window.
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

	// HMR for renderer base on electron-vite cli.
	// Load the remote URL for development or the local html file for production.
	if (is.dev && process.env.ELECTRON_RENDERER_URL) {
		mainWindow.loadURL(process.env.ELECTRON_RENDERER_URL);
	} else {
		mainWindow.loadFile(join(__dirname, "../renderer/index.html"));
	}
}

// This method will be called when Electron has finished
// initialization and is ready to create browser windows.
// Some APIs can only be used after this event occurs.
app.whenReady().then(() => {
	// Set app user model id for windows
	electronApp.setAppUserModelId("com.ominit.agent");

	// Default open or close DevTools by F12 in development
	// and ignore CommandOrControl + R in production.
	// see https://github.com/alex8088/electron-toolkit/tree/master/packages/utils
	app.on("browser-window-created", (_, window) => {
		optimizer.watchWindowShortcuts(window);
	});

	createWindow();

	// Start config watcher
	configWatcher = new ConfigWatcher(fileHandler, (data) => {
		console.log("Config file changed, reloading...");
		// Notify renderer process to reload config
		const windows = BrowserWindow.getAllWindows();
		windows.forEach((win) => {
			win.webContents.send("config:reload", Array.from(data));
		});
	});
	configWatcher.start();

	app.on("activate", () => {
		// On macOS it's common to re-create a window in the app when the
		// dock icon is clicked and there are no other windows open.
		if (BrowserWindow.getAllWindows().length === 0) createWindow();
	});
});

// Stop config watcher when quitting
app.on("before-quit", () => {
	configWatcher?.stop();
});

// Quit when all windows are closed, except on macOS. There, it's common
// for applications and their menu bar to stay active until the user quits
// explicitly with Cmd + Q.
app.on("window-all-closed", () => {
	if (process.platform !== "darwin") {
		app.quit();
	}
});

// In this file you can include the rest of your app's specific main process
// code. You can also put them in separate files and require them here.
