import { ElectronAPI } from "@electron-toolkit/preload";
import type {
	Event,
	Model,
	Session,
	SettingsDocument,
	Message,
} from "agent-ts-bindings";

type EventHandler = (event: Event) => void;

interface API {
	initRuntime: (onEvent: EventHandler) => Promise<void>;
	listModels: () => Promise<Record<string, Model[]>>;
	getSettings: () => Promise<SettingsDocument>;
	saveSettings: (settings: SettingsDocument) => Promise<void>;
	sendMessage: (
		message: string,
		modelId: string,
		providerId: string,
	) => Promise<void>;
	clearCurrentSession: () => void;
	getChatHistoryTree: () => Promise<Record<string, Message>>;
	approveTool: (callId: string) => Promise<void>;
	denyTool: (callId: string) => Promise<void>;
	executeApprovedTools: () => Promise<void>;
	listSessions: () => Promise<Session[]>;
	loadSession: (sessionId: string) => Promise<boolean>;
	deleteSession: (sessionId: string) => Promise<void>;
	getCurrentSessionId: () => Promise<string | null>;
	getCurrentWorkspaceState: () => Promise<{
		workspaceDir: string;
		appliesNextChat: boolean;
	} | null>;
	setCurrentWorkspaceDir: (workspaceDir: string) => Promise<void>;
	pickWorkspaceDir: (defaultPath?: string) => Promise<string | null>;
}

declare global {
	interface Window {
		electron: ElectronAPI;
		api: API;
	}
}

export { API };
