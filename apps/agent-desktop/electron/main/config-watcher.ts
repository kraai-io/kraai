import * as chokidar from "chokidar";
import * as path from "path";
import type { FileHandler } from "./file-handler";

export class ConfigWatcher {
	private watcher: chokidar.FSWatcher | null = null;
	private fileHandler: FileHandler;
	private onChange: () => Promise<void>;

	constructor(fileHandler: FileHandler, onChange: () => Promise<void>) {
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
			console.log("Config file change detected");
			await this.onChange();
		});

		console.log("Config watcher started for:", configPath);
	}

	stop(): void {
		this.watcher?.close();
		console.log("Config watcher stopped");
	}
}
