import { app } from "electron";
import * as fs from "fs/promises";
import * as path from "path";

export type FileResult<T> =
	| { ok: true; data: T }
	| { ok: false; error: FileError };

export type FileError =
	| { type: "NotFound"; path: string }
	| { type: "PermissionDenied"; path: string; operation: string }
	| { type: "InvalidPath"; path: string; reason: string }
	| { type: "IoError"; path: string; message: string }
	| { type: "UserCancelled" }
	| { type: "ParseError"; path: string; message: string };

export class FileHandler {
	private configDir: string;

	constructor() {
		// ~/.agent-desktop/ (Linux/macOS)
		// %USERPROFILE%\.agent-desktop\ (Windows)
		this.configDir = path.join(app.getPath("home"), ".agent-desktop");
		this.ensureDir(this.configDir);
	}

	getConfigDir(): string {
		return this.configDir;
	}

	// Config files - always allowed in ~/.agent-desktop/
	async readConfigFile(filename: string): Promise<FileResult<Uint8Array>> {
		try {
			const fullPath = path.join(this.configDir, filename);
			const data = await fs.readFile(fullPath);
			return { ok: true, data: new Uint8Array(data) };
		} catch (e: any) {
			if (e.code === "ENOENT") {
				return {
					ok: false,
					error: { type: "NotFound", path: filename },
				};
			}
			return {
				ok: false,
				error: { type: "IoError", path: filename, message: e.message },
			};
		}
	}

	async writeConfigFile(
		filename: string,
		data: Uint8Array,
	): Promise<FileResult<void>> {
		try {
			const fullPath = path.join(this.configDir, filename);
			await fs.writeFile(fullPath, Buffer.from(data));
			return { ok: true, data: undefined };
		} catch (e: any) {
			return {
				ok: false,
				error: { type: "IoError", path: filename, message: e.message },
			};
		}
	}

	// General file operations - full access for now (permissions later)
	async readFile(filePath: string): Promise<FileResult<Uint8Array>> {
		try {
			const data = await fs.readFile(filePath);
			return { ok: true, data: new Uint8Array(data) };
		} catch (e: any) {
			if (e.code === "ENOENT") {
				return {
					ok: false,
					error: { type: "NotFound", path: filePath },
				};
			}
			return {
				ok: false,
				error: { type: "IoError", path: filePath, message: e.message },
			};
		}
	}

	async writeFile(
		filePath: string,
		data: Uint8Array,
	): Promise<FileResult<void>> {
		try {
			await fs.writeFile(filePath, Buffer.from(data));
			return { ok: true, data: undefined };
		} catch (e: any) {
			return {
				ok: false,
				error: { type: "IoError", path: filePath, message: e.message },
			};
		}
	}

	async listDir(dirPath: string): Promise<FileResult<string[]>> {
		try {
			const entries = await fs.readdir(dirPath, { withFileTypes: true });
			return { ok: true, data: entries.map((e) => e.name) };
		} catch (e: any) {
			if (e.code === "ENOENT") {
				return {
					ok: false,
					error: { type: "NotFound", path: dirPath },
				};
			}
			return {
				ok: false,
				error: { type: "IoError", path: dirPath, message: e.message },
			};
		}
	}

	private async ensureDir(dir: string): Promise<void> {
		try {
			await fs.mkdir(dir, { recursive: true });
		} catch {
			// Directory might already exist
		}
	}

	// Create default config if it doesn't exist
	async ensureDefaultConfig(): Promise<void> {
		const configPath = path.join(this.configDir, "config.toml");
		try {
			await fs.access(configPath);
		} catch {
			// Create default config
			const defaultConfig = `# Default Agent Configuration
# Add your providers and models here

# Example provider:
# [[provider]]
# id = "openai"
# type = "openai"
# base_url = "https://api.openai.com/v1"
# env_var_api_key = "OPENAI_API_KEY"

# Example model:
# [[model]]
# id = "gpt-4"
# provider_id = "openai"
`;
			await fs.writeFile(configPath, defaultConfig, "utf-8");
		}
	}
}
