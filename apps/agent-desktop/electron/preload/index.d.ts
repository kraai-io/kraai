import { ElectronAPI } from "@electron-toolkit/preload";

interface API {
	plus100: (input: number) => number;
}

declare global {
	interface Window {
		electron: ElectronAPI;
		api: API;
	}
}

export { API };
