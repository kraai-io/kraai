import { electronAPI } from "@electron-toolkit/preload";
import { contextBridge, ipcRenderer } from "electron";

// Type definitions matching NAPI-RS Event enum
type Event =
	| { type: "ConfigLoaded" }
	| { type: "Error"; field0: string }
	| { type: "MessageComplete"; field0: string }
	| { type: "StreamStart"; messageId: string }
	| { type: "StreamChunk"; messageId: string; chunk: string }
	| { type: "StreamComplete"; messageId: string }
	| { type: "StreamError"; messageId: string; error: string }
	| {
			type: "ToolCallDetected";
			callId: string;
			toolId: string;
			args: string;
			description: string;
	  }
	| {
			type: "ToolResultReady";
			callId: string;
			toolId: string;
			success: boolean;
			output: string;
			denied: boolean;
	  }
	| { type: "HistoryUpdated" };

type EventHandler = (event: Event) => void;

// Matching NAPI-RS generated types (from index.d.ts)
interface Message {
	id: string;
	parentId?: string;
	role: number; // ChatRole const enum: System=0, User=1, Assistant=2, Tool=3
	content: string;
	status:
		| { type: "Complete" }
		| { type: "Streaming"; callId: string }
		| { type: "ProcessingTools" }
		| { type: "Cancelled" };
}

// API exposed to renderer
const api = {
	// Initialize the runtime with an event handler
	async initRuntime(onEvent: EventHandler): Promise<void> {
		// Set up event listener from main process
		ipcRenderer.on("agent:event", (_event, data: Event) => {
			console.log("[PRELOAD] Received event from main:", data.type);
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

	async getChatHistoryTree(): Promise<Record<string, Message>> {
		return await ipcRenderer.invoke("agent:getChatHistoryTree");
	},

	async approveTool(callId: string): Promise<void> {
		await ipcRenderer.invoke("agent:approveTool", callId);
	},

	async denyTool(callId: string): Promise<void> {
		await ipcRenderer.invoke("agent:denyTool", callId);
	},

	async executeApprovedTools(): Promise<void> {
		await ipcRenderer.invoke("agent:executeApprovedTools");
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
