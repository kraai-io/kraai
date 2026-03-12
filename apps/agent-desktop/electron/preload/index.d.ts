import { ElectronAPI } from "@electron-toolkit/preload";
import type {
	AgentProfilesState,
	Event,
	Model,
	ProviderDefinition,
	Session,
	SettingsDocument,
	Message,
	WorkspaceState,
} from "agent-ts-bindings";

type EventHandler = (event: Event) => void;

interface API {
	initRuntime: (onEvent: EventHandler) => Promise<void>;
	listModels: () => Promise<Record<string, Model[]>>;
	listProviderDefinitions: () => Promise<ProviderDefinition[]>;
	getSettings: () => Promise<SettingsDocument>;
	listAgentProfiles: (sessionId: string) => Promise<AgentProfilesState>;
	saveSettings: (settings: SettingsDocument) => Promise<void>;
	setSessionProfile: (sessionId: string, profileId: string) => Promise<void>;
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
	cancelStream: (sessionId: string) => Promise<boolean>;
	executeApprovedTools: (sessionId: string) => Promise<void>;
	listSessions: () => Promise<Session[]>;
	loadSession: (sessionId: string) => Promise<boolean>;
	deleteSession: (sessionId: string) => Promise<void>;
	getWorkspaceState: (sessionId: string) => Promise<WorkspaceState | null>;
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
