import { electronAPI } from "@electron-toolkit/preload";
import { AgentApi, type FileError } from "agent-ts-bindings";
import { contextBridge, ipcRenderer } from "electron";

// Store the AgentApi instance globally so we can reload config
let agentApi: AgentApi | null = null;

// Factory function to create AgentApi with file callbacks
const createAgentApi = () => {
	// File read callback
	const readFileCallback = (
		_err: Error | null,
		filePath: string,
	): FileError | Uint8Array => {
		// This runs synchronously but calls async IPC
		// Note: In production, this should be async, but for now we use sync IPC
		const result = ipcRenderer.sendSync("file:read", filePath);
		if (result.ok) {
			return new Uint8Array(result.data);
		} else {
			return result.error;
		}
	};

	// File write callback
	const writeFileCallback = (
		_err: Error | null,
		filePath: string,
		data: Uint8Array,
	): FileError | undefined => {
		const result = ipcRenderer.sendSync(
			"file:write",
			filePath,
			Array.from(data),
		);
		if (result.ok) {
			return undefined;
		} else {
			return result.error;
		}
	};

	// List dir callback
	const listDirCallback = (
		_err: Error | null,
		dirPath: string,
	): FileError | string[] => {
		const result = ipcRenderer.sendSync("file:listDir", dirPath);
		if (result.ok) {
			return result.entries;
		} else {
			return result.error;
		}
	};

	agentApi = new AgentApi(readFileCallback, writeFileCallback, listDirCallback);
	return agentApi;
};

// Handle config reload events from main process
ipcRenderer.on(
	"config:reload",
	async (_event, payload: { data: number[]; configDir: string }) => {
		if (agentApi) {
			try {
				const configData = new Uint8Array(payload.data);
				await agentApi.reloadConfig(configData, payload.configDir);
				console.log("Config reloaded successfully");
			} catch (error) {
				console.error("Failed to reload config:", error);
			}
		} else {
			console.warn("Received config:reload but AgentApi not initialized yet");
		}
	},
);

// Initialize AgentApi immediately with file callbacks
createAgentApi();

// Custom APIs for renderer
const api = {
	createAgentApi,
	// Expose agentApi instance for direct access
	get agentApi() {
		return agentApi;
	},
};

// Use `contextBridge` APIs to expose Electron APIs to
// renderer only if context isolation is enabled, otherwise
// just add to the DOM global.
if (process.contextIsolated) {
	try {
		contextBridge.exposeInMainWorld("electron", electronAPI);
		contextBridge.exposeInMainWorld("api", api);
	} catch (error) {
		console.error(error);
	}
} else {
	// @ts-expect-error (define in dts)
	window.electron = electronAPI;
	// @ts-expect-error (define in dts)
	window.api = api;
}
