import { ElectronAPI } from "@electron-toolkit/preload";
import type { AgentApi } from "agent-ts-bindings";

interface API {
	plus100: (input: number) => number;
	createAgentApi: () => AgentApi;
}

declare global {
	interface Window {
		electron: ElectronAPI;
		api: API;
	}
}

export { API };
