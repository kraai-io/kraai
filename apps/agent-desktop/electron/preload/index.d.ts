import { ElectronAPI } from "@electron-toolkit/preload";

interface Event {
	eventType: string;
	field0?: string;
}

type EventHandler = (event: Event) => void;

interface API {
	initRuntime: (onEvent: EventHandler) => Promise<void>;
	listModels: () => Promise<string[]>;
	sendMessage: (
		message: string,
		modelId: string,
		providerId: string,
	) => Promise<void>;
}

declare global {
	interface Window {
		electron: ElectronAPI;
		api: API;
	}
}

export { API };
