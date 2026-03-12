import { electronAPI } from "@electron-toolkit/preload";
import type {
	AgentProfilesState,
	Message,
	Model,
	ProviderDefinition,
	Session,
	SettingsDocument,
	WorkspaceState,
} from "agent-ts-bindings";
import { contextBridge, ipcRenderer } from "electron";

// Type definitions matching NAPI-RS Event enum
type Event =
	| { type: "ConfigLoaded" }
	| { type: "Error"; field0: string }
	| { type: "MessageComplete"; field0: string }
	| { type: "StreamStart"; sessionId: string; messageId: string }
	| {
			type: "StreamChunk";
			sessionId: string;
			messageId: string;
			chunk: string;
	  }
	| { type: "StreamComplete"; sessionId: string; messageId: string }
	| {
			type: "StreamError";
			sessionId: string;
			messageId: string;
			error: string;
	  }
	| { type: "StreamCancelled"; sessionId: string; messageId: string }
	| {
			type: "ToolCallDetected";
			sessionId: string;
			callId: string;
			toolId: string;
			args: string;
			description: string;
			riskLevel: string;
			reasons: string[];
	  }
	| {
			type: "ToolResultReady";
			sessionId: string;
			callId: string;
			toolId: string;
			success: boolean;
			output: string;
			denied: boolean;
	  }
	| {
			type: "ContinuationFailed";
			sessionId: string;
			error: string;
	  }
	| { type: "HistoryUpdated"; sessionId: string };

type EventHandler = (event: Event) => void;

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
	async listModels(): Promise<Record<string, Model[]>> {
		return await ipcRenderer.invoke("agent:listModels");
	},

	async getSettings(): Promise<SettingsDocument> {
		return await ipcRenderer.invoke("agent:getSettings");
	},

	async listAgentProfiles(sessionId: string): Promise<AgentProfilesState> {
		return await ipcRenderer.invoke("agent:listAgentProfiles", sessionId);
	},

	async listProviderDefinitions(): Promise<ProviderDefinition[]> {
		return await ipcRenderer.invoke("agent:listProviderDefinitions");
	},

	async setSessionProfile(sessionId: string, profileId: string): Promise<void> {
		await ipcRenderer.invoke("agent:setSessionProfile", sessionId, profileId);
	},

	async saveSettings(settings: SettingsDocument): Promise<void> {
		await ipcRenderer.invoke("agent:saveSettings", settings);
	},

	async createSession(): Promise<string> {
		return await ipcRenderer.invoke("agent:createSession");
	},

	async sendMessage(
		sessionId: string,
		message: string,
		modelId: string,
		providerId: string,
	): Promise<void> {
		await ipcRenderer.invoke(
			"agent:sendMessage",
			sessionId,
			message,
			modelId,
			providerId,
		);
	},

	async getChatHistoryTree(
		sessionId: string,
	): Promise<Record<string, Message>> {
		return await ipcRenderer.invoke("agent:getChatHistoryTree", sessionId);
	},

	async approveTool(sessionId: string, callId: string): Promise<void> {
		await ipcRenderer.invoke("agent:approveTool", sessionId, callId);
	},

	async denyTool(sessionId: string, callId: string): Promise<void> {
		await ipcRenderer.invoke("agent:denyTool", sessionId, callId);
	},

	async cancelStream(sessionId: string): Promise<boolean> {
		return await ipcRenderer.invoke("agent:cancelStream", sessionId);
	},

	async executeApprovedTools(sessionId: string): Promise<void> {
		await ipcRenderer.invoke("agent:executeApprovedTools", sessionId);
	},

	async listSessions(): Promise<Session[]> {
		return await ipcRenderer.invoke("agent:listSessions");
	},

	async loadSession(sessionId: string): Promise<boolean> {
		return await ipcRenderer.invoke("agent:loadSession", sessionId);
	},

	async deleteSession(sessionId: string): Promise<void> {
		await ipcRenderer.invoke("agent:deleteSession", sessionId);
	},

	async getWorkspaceState(sessionId: string): Promise<WorkspaceState | null> {
		return await ipcRenderer.invoke("agent:getWorkspaceState", sessionId);
	},

	async setWorkspaceDir(
		sessionId: string,
		workspaceDir: string,
	): Promise<void> {
		await ipcRenderer.invoke("agent:setWorkspaceDir", sessionId, workspaceDir);
	},

	async pickWorkspaceDir(defaultPath?: string): Promise<string | null> {
		return await ipcRenderer.invoke("agent:pickWorkspaceDir", defaultPath);
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
