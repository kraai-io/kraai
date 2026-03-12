import { ElectronAPI } from "@electron-toolkit/preload";
import type {
	Event,
	Model,
	ProviderDefinition,
	Session,
	SettingsDocument,
	Message,
} from "agent-ts-bindings";

type EventHandler = (event: Event) => void;

interface API {
	initRuntime: (onEvent: EventHandler) => Promise<void>;
	listModels: () => Promise<Record<string, Model[]>>;
	listProviderDefinitions: () => Promise<ProviderDefinition[]>;
	getSettings: () => Promise<SettingsDocument>;
	saveSettings: (settings: SettingsDocument) => Promise<void>;
	createSession: () => Promise<string>;
	sendMessage: (
		sessionId: string,
		message: string,
		modelId: string,
		providerId: string,
	) => Promise<void>;
	getChatHistoryTree: (sessionId: string) => Promise<Record<string, Message>>;
	approveTool: (sessionId: string, callId: string) => Promise<void>;
	denyTool: (sessionId: string, callId: string) => Promise<void>;
	executeApprovedTools: (sessionId: string) => Promise<void>;
	listSessions: () => Promise<Session[]>;
	loadSession: (sessionId: string) => Promise<boolean>;
	deleteSession: (sessionId: string) => Promise<void>;
	getWorkspaceState: (sessionId: string) => Promise<{
		workspaceDir: string;
		appliesNextChat: boolean;
	} | null>;
	setWorkspaceDir: (sessionId: string, workspaceDir: string) => Promise<void>;
	pickWorkspaceDir: (defaultPath?: string) => Promise<string | null>;
}

declare global {
	interface Window {
		electron: ElectronAPI;
		api: API;
	}
}

export { API };
