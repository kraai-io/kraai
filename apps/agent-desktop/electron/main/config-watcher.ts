import * as chokidar from "chokidar";
import * as path from "path";
import type { FileHandler } from "./file-handler";

export class ConfigWatcher {
	private watcher: chokidar.FSWatcher | null = null;
	private fileHandler: FileHandler;
	private onChange: (data: Uint8Array) => void;

	constructor(fileHandler: FileHandler, onChange: (data: Uint8Array) => void) {
		this.fileHandler = fileHandler;
		this.onChange = onChange;
	}

	start(): void {
		const configPath = path.join(
			this.fileHandler.getConfigDir(),
			"config.toml",
		);

		this.watcher = chokidar.watch(configPath, {
			persistent: true,
			ignoreInitial: true,
		});

		this.watcher.on("change", async () => {
			try {
				const result = await this.fileHandler.readConfigFile("config.toml");
				if (result.ok) {
					this.onChange(result.data);
				} else {
					console.error("Failed to reload config:", result.error);
				}
			} catch (e) {
				console.error("Error reloading config:", e);
			}
		});

		console.log("Config watcher started for:", configPath);
	}

	stop(): void {
		this.watcher?.close();
		console.log("Config watcher stopped");
	}
}
