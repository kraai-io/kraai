import { electronAPI } from "@electron-toolkit/preload";
import { contextBridge, ipcRenderer } from "electron";

// Type definitions for the API
interface Event {
	eventType: string;
	data?: string;
}

type EventHandler = (event: Event) => void;

// API exposed to renderer
const api = {
	// Initialize the runtime with an event handler
	async initRuntime(onEvent: EventHandler): Promise<void> {
		// Set up event listener from main process
		ipcRenderer.on("agent:event", (_event, data: Event) => {
			onEvent(data);
		});
	},

	// Async methods that call into Rust via main process
	async listModels(): Promise<string[]> {
		return await ipcRenderer.invoke("agent:listModels");
	},

	async sendMessage(
		message: string,
		modelId: string,
		providerId: string,
	): Promise<void> {
		await ipcRenderer.invoke("agent:sendMessage", message, modelId, providerId);
	},

	async newSession(): Promise<void> {
		await ipcRenderer.invoke("agent:newSession");
	},
};

// Expose APIs
if (process.contextIsolated) {
	try {
		contextBridge.exposeInMainWorld("electron", electronAPI);
		contextBridge.exposeInMainWorld("api", api);
	} catch (error) {
		console.error("[PRELOAD] Failed to expose APIs:", error);
	}
} else {
	// @ts-expect-error
	window.electron = electronAPI;
	// @ts-expect-error
	window.api = api;
}
