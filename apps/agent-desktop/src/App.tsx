import type {
	Model as BindingModel,
	Event,
	SettingsDocument,
} from "agent-ts-bindings";
import {
	Bot,
	ChevronDown,
	ChevronLeft,
	ChevronRight,
	Plus,
	Send,
	Settings2,
	Square,
	Trash2,
	Wrench,
} from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { ChatMessage } from "@/components/chat-message";
import { ModelSelector } from "@/components/model-selector";
import { SettingsDialog } from "@/components/settings-dialog";
import { Button } from "@/components/ui/button";
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from "@/components/ui/dialog";

interface Message {
	id: string;
	parentId?: string;
	role: number;
	content: string;
	status:
		| { type: "Complete" }
		| { type: "Streaming"; callId: string }
		| { type: "ProcessingTools" }
		| { type: "Cancelled" };
}

interface UIMessage {
	id: string;
	content: string;
	role: "user" | "assistant" | "tool";
	isStreaming: boolean;
}

interface PendingTool {
	callId: string;
	toolId: string;
	args: string;
	description: string;
	riskLevel: string;
	reasons: string[];
	approved: boolean | null;
}

interface Model extends BindingModel {
	providerId: string;
}

interface Session {
	id: string;
	tipId?: string;
	createdAt: number;
	updatedAt: number;
	title?: string;
}

interface WindowAPI {
	initRuntime: (callback: (event: Event) => void) => void;
	listModels: () => Promise<Record<string, BindingModel[]>>;
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
}

declare global {
	interface Window {
		api: WindowAPI;
	}
}

function App(): React.JSX.Element {
	const [messages, setMessages] = useState<UIMessage[]>([]);
	const [inputValue, setInputValue] = useState("");
	const [isLoading, setIsLoading] = useState(false);
	const [models, setModels] = useState<Model[]>([]);
	const [selectedModel, setSelectedModel] = useState<[string, string] | null>(
		null,
	);
	const [pendingTools, setPendingTools] = useState<PendingTool[]>([]);
	const [sessions, setSessions] = useState<Session[]>([]);
	const [currentSessionId, setCurrentSessionId] = useState<string | null>(null);
	const [isSidebarOpen, setIsSidebarOpen] = useState(false);
	const [sessionToDelete, setSessionToDelete] = useState<string | null>(null);
	const [isSettingsOpen, setIsSettingsOpen] = useState(false);
	const scrollRef = useRef<HTMLDivElement>(null);
	const textareaRef = useRef<HTMLTextAreaElement>(null);
	const isInitializedRef = useRef(false);
	const [isAtBottom, setIsAtBottom] = useState(true);
	const isAtBottomRef = useRef(true);

	const scrollToBottom = useCallback(() => {
		if (scrollRef.current && isAtBottomRef.current) {
			requestAnimationFrame(() => {
				if (scrollRef.current) {
					scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
				}
			});
		}
	}, []);

	const forceScrollToBottom = useCallback(() => {
		if (scrollRef.current) {
			requestAnimationFrame(() => {
				if (scrollRef.current) {
					scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
				}
			});
		}
	}, []);

	const loadModels = useCallback(async () => {
		const api = window.api;
		if (!api) return;

		try {
			const modelMap = await api.listModels();
			const allModels: Model[] = [];

			for (const [providerId, providerModels] of Object.entries(modelMap)) {
				for (const model of providerModels) {
					allModels.push({ ...model, providerId });
				}
			}

			setModels(allModels);
			if (allModels.length > 0 && !selectedModel) {
				setSelectedModel([allModels[0].providerId, allModels[0].id]);
			}
		} catch (err) {
			console.error("[UI] Failed to load models:", err);
		}
	}, [selectedModel]);

	const loadSessions = useCallback(async () => {
		const api = window.api;
		if (!api) return;

		try {
			const [sessionsList, currentId] = await Promise.all([
				api.listSessions(),
				api.getCurrentSessionId(),
			]);
			setSessions(sessionsList);
			setCurrentSessionId(currentId);
		} catch (err) {
			console.error("[UI] Failed to load sessions:", err);
		}
	}, []);

	const loadChatHistory = useCallback(async () => {
		const api = window.api;
		if (!api) return;

		try {
			const historyMap = await api.getChatHistoryTree();
			const entries = Object.entries(historyMap);
			console.log("[UI] Chat history loaded:", entries.length, "messages");

			const messageMap = new Map<string, Message>();
			const childMap = new Map<string, string>();

			for (const [id, msg] of entries) {
				console.log(
					"[UI] Message:",
					id,
					"role:",
					msg.role,
					"parent:",
					msg.parentId,
				);
				messageMap.set(id, msg);
				if (msg.parentId) {
					childMap.set(msg.parentId, id);
				}
			}

			let tipId: string | null = null;
			for (const [id] of entries) {
				if (!childMap.has(id)) {
					tipId = id;
					break;
				}
			}

			const ordered: { id: string; msg: Message }[] = [];
			let currentId = tipId;
			while (currentId) {
				const msg = messageMap.get(currentId);
				if (msg) {
					ordered.unshift({ id: currentId, msg });
					currentId = msg.parentId || null;
				} else break;
			}

			const mappedMessages: UIMessage[] = ordered
				.filter(({ msg }) => msg.role === 1 || msg.role === 2 || msg.role === 3)
				.map(({ id, msg }) => ({
					id,
					content: msg.content,
					role: msg.role === 1 ? "user" : msg.role === 2 ? "assistant" : "tool",
					isStreaming: msg.status.type === "Streaming",
				}));

			setMessages(mappedMessages);
			forceScrollToBottom();
		} catch (err) {
			console.error("[UI] Failed to load chat history:", err);
		}
	}, [forceScrollToBottom]);

	useEffect(() => {
		const api = window.api;
		if (!api || isInitializedRef.current) return;
		isInitializedRef.current = true;

		api.initRuntime((event: Event) => {
			console.log("[UI] Event:", event.type);

			switch (event.type) {
				case "ConfigLoaded":
					loadModels();
					loadSessions();
					loadChatHistory();
					break;
				case "Error":
					console.error("[UI] Error:", event.field0);
					setIsLoading(false);
					break;
				case "StreamStart":
					setMessages((prev) => [
						...prev,
						{
							id: event.messageId,
							content: "",
							role: "assistant",
							isStreaming: true,
						},
					]);
					forceScrollToBottom();
					break;
				case "StreamChunk":
					setMessages((prev) =>
						prev.map((msg) =>
							msg.id === event.messageId
								? { ...msg, content: msg.content + event.chunk }
								: msg,
						),
					);
					scrollToBottom();
					break;
				case "StreamComplete":
					setIsLoading(false);
					loadChatHistory();
					break;
				case "StreamError":
					console.error("[UI] Stream error:", event.error);
					setIsLoading(false);
					setMessages((prev) =>
						prev.filter((msg) => msg.id !== event.messageId),
					);
					break;
				case "ToolCallDetected":
					console.log(
						"[UI] Tool call detected:",
						event.toolId,
						event.description,
					);
					setPendingTools((prev) => [
						...prev,
						{
							callId: event.callId,
							toolId: event.toolId,
							args: event.args,
							description: event.description,
							riskLevel: event.riskLevel,
							reasons: event.reasons,
							approved: null,
						},
					]);
					break;
				case "ToolResultReady":
					console.log(
						"[UI] Tool result ready:",
						event.toolId,
						event.success,
						event.denied,
					);
					setPendingTools((prev) =>
						prev.filter((t) => t.callId !== event.callId),
					);
					break;
				case "HistoryUpdated":
					console.log("[UI] HistoryUpdated event received");
					loadChatHistory();
					loadSessions();
					break;
			}
		});

		loadModels();
	}, [
		loadModels,
		loadChatHistory,
		forceScrollToBottom,
		scrollToBottom,
		loadSessions,
	]);

	useEffect(() => {
		const container = scrollRef.current;
		if (!container) return;

		const handleScroll = () => {
			const { scrollTop, scrollHeight, clientHeight } = container;
			const atBottom = scrollTop + clientHeight >= scrollHeight - 100;
			setIsAtBottom(atBottom);
			isAtBottomRef.current = atBottom;
		};

		container.addEventListener("scroll", handleScroll);
		return () => container.removeEventListener("scroll", handleScroll);
	}, []);

	const handleSend = async () => {
		if (!inputValue.trim() || isLoading || !selectedModel) return;

		const [providerId, modelId] = selectedModel;
		const content = inputValue.trim();
		setInputValue("");
		setIsLoading(true);

		const optimisticMessage: UIMessage = {
			id: Date.now().toString(),
			content,
			role: "user",
			isStreaming: false,
		};
		setMessages((prev) => [...prev, optimisticMessage]);
		forceScrollToBottom();

		window.api?.sendMessage(content, modelId, providerId).catch((err) => {
			console.error("[UI] Send failed:", err);
			setIsLoading(false);
		});
	};

	const handleKeyDown = (e: React.KeyboardEvent) => {
		if (e.key === "Enter" && !e.shiftKey) {
			e.preventDefault();
			handleSend();
		}
	};

	const handleNewChat = () => {
		setMessages([]);
		setPendingTools([]);
		setCurrentSessionId(null);
		window.api?.clearCurrentSession();
	};

	const handleLoadSession = async (sessionId: string) => {
		const success = await window.api?.loadSession(sessionId);
		if (success) {
			setCurrentSessionId(sessionId);
			setIsSidebarOpen(false);
			await loadChatHistory();
		}
	};

	const handleDeleteSession = async (sessionId: string) => {
		await window.api?.deleteSession(sessionId);
		setSessions((prev) => prev.filter((s) => s.id !== sessionId));
		if (sessionId === currentSessionId) {
			setMessages([]);
			setCurrentSessionId(null);
		}
		setSessionToDelete(null);
	};

	const handleApproveTool = async (callId: string) => {
		await window.api?.approveTool(callId);
		setPendingTools((prev) =>
			prev.map((t) => (t.callId === callId ? { ...t, approved: true } : t)),
		);
	};

	const handleDenyTool = async (callId: string) => {
		await window.api?.denyTool(callId);
		setPendingTools((prev) =>
			prev.map((t) => (t.callId === callId ? { ...t, approved: false } : t)),
		);
	};

	const handleExecuteTools = async () => {
		await window.api?.executeApprovedTools();
	};

	const unhandledTools = pendingTools.filter((t) => t.approved === null);
	const hasApprovedTools = pendingTools.some((t) => t.approved === true);

	return (
		<div className="flex h-screen bg-background">
			<SettingsDialog
				open={isSettingsOpen}
				onOpenChange={setIsSettingsOpen}
				onSaved={() => {
					loadModels();
				}}
			/>

			{/* Delete Session Dialog */}
			<Dialog open={sessionToDelete !== null}>
				<DialogContent>
					<DialogHeader>
						<DialogTitle>Delete Session</DialogTitle>
						<DialogDescription>
							Are you sure you want to delete this session? This action cannot
							be undone.
						</DialogDescription>
					</DialogHeader>
					<DialogFooter>
						<Button variant="outline" onClick={() => setSessionToDelete(null)}>
							Cancel
						</Button>
						<Button
							variant="destructive"
							onClick={() =>
								sessionToDelete && handleDeleteSession(sessionToDelete)
							}
						>
							Delete
						</Button>
					</DialogFooter>
				</DialogContent>
			</Dialog>

			{/* Sidebar */}
			<div
				className={`border-r transition-all duration-300 ${
					isSidebarOpen ? "w-64" : "w-0"
				} overflow-hidden`}
			>
				<div className="flex h-full flex-col">
					<div className="flex items-center justify-between border-b p-4">
						<h2 className="font-semibold">Sessions</h2>
						<Button
							variant="ghost"
							size="icon"
							className="h-8 w-8"
							onClick={() => setIsSidebarOpen(false)}
						>
							<ChevronLeft className="h-4 w-4" />
						</Button>
					</div>
					<div className="flex-1 overflow-y-auto">
						<div className="p-2">
							<Button
								variant="ghost"
								className="w-full justify-start gap-2"
								onClick={handleNewChat}
							>
								<Plus className="h-4 w-4" />
								New Chat
							</Button>
						</div>
						<div className="p-2 pt-0">
							{sessions.map((session) => (
								<button
									type="button"
									key={session.id}
									className={`group flex w-full items-center gap-2 rounded-md p-2 text-left ${
										session.id === currentSessionId
											? "bg-accent"
											: "hover:bg-muted"
									}`}
									onClick={() => handleLoadSession(session.id)}
								>
									<div className="flex-1 truncate text-sm">
										{session.title || `Session ${session.id.slice(0, 8)}`}
									</div>
									<Button
										variant="ghost"
										size="icon"
										className="h-6 w-6 opacity-0 group-hover:opacity-100"
										onClick={(e) => {
											e.stopPropagation();
											setSessionToDelete(session.id);
										}}
									>
										<Trash2 className="h-3 w-3" />
									</Button>
								</button>
							))}
							{sessions.length === 0 && (
								<p className="p-2 text-sm text-muted-foreground">
									No sessions yet
								</p>
							)}
						</div>
					</div>
				</div>
			</div>

			{/* Main Content */}
			<div className="flex flex-1 flex-col">
				{/* Tool Permission Dialog */}
				<Dialog open={unhandledTools.length > 0}>
					<DialogContent showCloseButton={false}>
						<DialogHeader>
							<DialogTitle className="flex items-center gap-2">
								<Wrench className="h-5 w-5" />
								Tool Permission Request
							</DialogTitle>
							<DialogDescription>
								The assistant wants to execute the following tool:
							</DialogDescription>
						</DialogHeader>
						{unhandledTools[0] && (
							<div className="space-y-4">
								<div className="rounded-lg border bg-muted/50 p-4">
									<div className="font-mono text-sm font-medium">
										{unhandledTools[0].toolId}
									</div>
									<div className="text-muted-foreground mt-1">
										{unhandledTools[0].description}
									</div>
									<div className="mt-2 text-xs uppercase tracking-wide text-muted-foreground">
										Risk: {unhandledTools[0].riskLevel.replaceAll("_", " ")}
									</div>
									{unhandledTools[0].reasons.length > 0 && (
										<div className="mt-2 whitespace-pre-line text-xs text-muted-foreground">
											{unhandledTools[0].reasons.join("\n")}
										</div>
									)}
									{unhandledTools[0].args &&
										unhandledTools[0].args !== "{}" && (
											<pre className="mt-2 text-xs bg-background rounded p-2 overflow-x-auto">
												{JSON.stringify(
													JSON.parse(unhandledTools[0].args),
													null,
													2,
												)}
											</pre>
										)}
								</div>
								<DialogFooter>
									<Button
										variant="outline"
										onClick={() => handleDenyTool(unhandledTools[0].callId)}
									>
										Deny
									</Button>
									<Button
										onClick={() => handleApproveTool(unhandledTools[0].callId)}
									>
										Approve
									</Button>
								</DialogFooter>
							</div>
						)}
					</DialogContent>
				</Dialog>

				{/* Execute Tools Button */}
				{hasApprovedTools && unhandledTools.length === 0 && (
					<div className="fixed bottom-24 left-1/2 -translate-x-1/2 z-20">
						<Button onClick={handleExecuteTools} className="shadow-lg">
							Execute Approved Tools
						</Button>
					</div>
				)}
				<header className="flex items-center justify-between border-b px-4 py-3">
					<div className="flex items-center gap-2">
						<Button
							variant="ghost"
							size="icon"
							onClick={() => setIsSidebarOpen(!isSidebarOpen)}
							title="Toggle sessions"
						>
							<ChevronRight
								className={`h-4 w-4 transition-transform ${
									isSidebarOpen ? "rotate-180" : ""
								}`}
							/>
						</Button>
						<div className="flex h-8 w-8 items-center justify-center rounded-lg bg-primary">
							<Bot className="h-4 w-4 text-primary-foreground" />
						</div>
						<h1 className="text-lg font-semibold tracking-tight">Agent</h1>
					</div>
					<Button
						variant="ghost"
						size="icon"
						onClick={() => setIsSettingsOpen(true)}
						title="Settings"
					>
						<Settings2 className="h-4 w-4" />
					</Button>
					<Button
						variant="ghost"
						size="icon"
						onClick={handleNewChat}
						title="New chat"
					>
						<Plus className="h-4 w-4" />
					</Button>
				</header>

				<div className="relative flex-1">
					<div ref={scrollRef} className="absolute inset-0 overflow-y-auto">
						<div className="mx-auto max-w-2xl px-4">
							{messages.length === 0 ? (
								<div className="flex h-[60vh] flex-col items-center justify-center text-muted-foreground">
									<div className="flex h-14 w-14 items-center justify-center rounded-xl bg-muted mb-4">
										<Bot className="h-7 w-7" />
									</div>
									<p className="text-base">Send a message to start</p>
								</div>
							) : (
								<div className="divide-y">
									{messages.map((message) => (
										<ChatMessage
											key={message.id}
											content={message.content}
											role={message.role}
											isStreaming={message.isStreaming}
										/>
									))}
								</div>
							)}
						</div>
					</div>

					{!isAtBottom && (
						<div className="absolute bottom-0 left-1/2 -translate-x-1/2 z-10 pb-2">
							<Button
								variant="secondary"
								size="icon"
								className="h-8 w-8 rounded-full shadow-md"
								onClick={() => {
									forceScrollToBottom();
									setIsAtBottom(true);
									isAtBottomRef.current = true;
								}}
							>
								<ChevronDown className="h-4 w-4" />
							</Button>
						</div>
					)}
				</div>

				<div className="border-t p-4">
					<div className="mx-auto max-w-2xl">
						<div className="rounded-2xl border p-3">
							<textarea
								ref={textareaRef as React.RefObject<HTMLTextAreaElement>}
								value={inputValue}
								onChange={(e) => {
									setInputValue(e.target.value);
									// Auto-resize
									e.target.style.height = "auto";
									e.target.style.height = `${e.target.scrollHeight}px`;
								}}
								onKeyDown={(e) => {
									handleKeyDown(e);
									// Reset height on backspace if needed
									if (e.key === "Backspace") {
										const target = e.target as HTMLTextAreaElement;
										target.style.height = "auto";
										target.style.height = `${target.scrollHeight}px`;
									}
								}}
								placeholder="Message..."
								className="w-full resize-none bg-transparent px-2 py-1 text-base outline-none placeholder:text-muted-foreground"
								rows={1}
								style={{ height: "auto", overflow: "hidden" }}
								disabled={isLoading}
							/>
							<div className="flex items-center justify-between pt-2">
								<ModelSelector
									models={models}
									value={selectedModel}
									onChange={setSelectedModel}
								/>
								<Button
									size="icon"
									className="h-8 w-8 rounded-xl"
									onClick={handleSend}
									disabled={!inputValue.trim() || isLoading || !selectedModel}
								>
									{isLoading ? (
										<Square className="h-3.5 w-3.5" />
									) : (
										<Send className="h-3.5 w-3.5" />
									)}
								</Button>
							</div>
						</div>
					</div>
				</div>
			</div>
		</div>
	);
}

export default App;
