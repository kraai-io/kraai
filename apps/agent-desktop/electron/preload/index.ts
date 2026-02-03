import { electronAPI } from "@electron-toolkit/preload";
import { AgentApi, type FileError, plus100 } from "agent-ts-bindings";
import { contextBridge, ipcRenderer } from "electron";

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

	return new AgentApi(readFileCallback, writeFileCallback, listDirCallback);
};

// Custom APIs for renderer
const api = {
	plus100,
	createAgentApi,
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
