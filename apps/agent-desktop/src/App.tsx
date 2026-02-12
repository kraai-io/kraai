import type { Model as BindingModel, Event } from "agent-ts-bindings";
import { Bot, Loader2, Search, Send, Trash2 } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { ChatMessage } from "@/components/chat-message";
import { Button } from "@/components/ui/button";
import {
	Select,
	SelectContent,
	SelectGroup,
	SelectItem,
	SelectLabel,
	SelectTrigger,
} from "@/components/ui/select";
import { Separator } from "@/components/ui/separator";
import { Textarea } from "@/components/ui/textarea";

interface Message {
	id: string;
	content: string;
	role: "user" | "assistant";
	timestamp: Date;
}

interface Model extends BindingModel {
	providerId: string;
}

interface WindowAPI {
	initRuntime: (callback: (event: Event) => void) => void;
	listModels: () => Promise<Record<string, BindingModel[]>>;
	sendMessage: (
		message: string,
		modelId: string,
		providerId: string,
	) => Promise<void>;
	newSession: () => Promise<void>;
	getChatHistory: () => Promise<Array<{ role: number; content: string }>>;
}

declare global {
	interface Window {
		api?: WindowAPI;
	}
}

const serializeModelKey = (providerId: string, modelId: string): string =>
	`${providerId}::${modelId}`;

const deserializeModelKey = (key: string): [string, string] => {
	const parts = key.split("::");
	return [parts[0], parts[1] || ""];
};

function App(): React.JSX.Element {
	const [messages, setMessages] = useState<Message[]>([]);
	const [inputValue, setInputValue] = useState("");
	const [isLoading, setIsLoading] = useState(false);
	const [models, setModels] = useState<Model[]>([]);
	const [selectedModel, setSelectedModel] = useState<[string, string] | null>(
		null,
	);
	const [pendingMessageId, setPendingMessageId] = useState<string | null>(null);
	const [searchQuery, setSearchQuery] = useState("");
	const [isSelectorOpen, setIsSelectorOpen] = useState(false);
	const pendingMessageIdRef = useRef<string | null>(null);
	const scrollRef = useRef<HTMLDivElement>(null);
	const textareaRef = useRef<HTMLTextAreaElement>(null);
	const isAtBottomRef = useRef(true);
	const isInitializedRef = useRef(false);
	const selectTriggerRef = useRef<HTMLButtonElement>(null);
	const textareaHeightRef = useRef<number>(0);

	useEffect(() => {
		pendingMessageIdRef.current = pendingMessageId;
	}, [pendingMessageId]);

	useEffect(() => {
		const textarea = textareaRef.current;
		const scrollContainer = scrollRef.current;
		if (!textarea || !scrollContainer) return;

		const resizeObserver = new ResizeObserver((entries) => {
			for (const entry of entries) {
				const newHeight = entry.contentRect.height;
				const oldHeight = textareaHeightRef.current;

				if (oldHeight > 0 && newHeight !== oldHeight) {
					const heightDiff = newHeight - oldHeight;
					// Adjust scroll position to maintain the same view
					scrollContainer.scrollTop += heightDiff;
				}

				textareaHeightRef.current = newHeight;
			}
		});

		textareaHeightRef.current = textarea.getBoundingClientRect().height;
		resizeObserver.observe(textarea);

		return () => {
			resizeObserver.disconnect();
		};
	}, []);

	useEffect(() => {
		const api = window.api;
		if (!api) return;
		if (isInitializedRef.current) return;
		isInitializedRef.current = true;

		api.initRuntime((event: Event) => {
			console.log("[UI] Received event from Rust:", event);

			if (event.type === "ConfigLoaded") {
				console.log("[UI] Config loaded");
				loadModels();
				loadChatHistory();
			} else if (event.type === "Error") {
				console.error("[UI] Error:", event.field0);
				setIsLoading(false);
			} else if (event.type === "MessageComplete") {
				console.log("[UI] Message complete:", event.field0);
				// Reload chat history from Rust (source of truth)
				loadChatHistory().then(() => {
					setIsLoading(false);
					setPendingMessageId(null);
				});
			} else {
				console.log(
					"[UI] Unknown event type:",
					(event as { type: string }).type,
				);
			}
		});

		loadModels();
	}, []);

	const loadModels = async () => {
		const api = window.api;
		if (!api) return;

		try {
			const modelMap: Record<
				string,
				Array<BindingModel>
			> = await api.listModels();
			console.log("[UI] Models loaded:", modelMap);

			const allModels: Model[] = [];
			for (const [providerId, providerModels] of Object.entries(modelMap)) {
				for (const model of providerModels) {
					allModels.push({
						id: model.id,
						name: model.name,
						providerId: providerId,
					});
				}
			}

			setModels(allModels);
			if (allModels.length > 0 && !selectedModel) {
				setSelectedModel([allModels[0].providerId, allModels[0].id]);
			}
		} catch (err) {
			console.error("[UI] Failed to load models:", err);
		}
	};

	const loadChatHistory = async () => {
		const api = window.api;
		if (!api) return;

		try {
			const history = await api.getChatHistory();
			console.log("[UI] Chat history loaded:", history);

			const mappedMessages: Message[] = history
				.filter((msg) => msg.role === 1 || msg.role === 2) // Only User (1) and Assistant (2)
				.map((msg, index) => ({
					id: `history-${index}`,
					content: msg.content,
					role: msg.role === 1 ? "user" : "assistant",
					timestamp: new Date(),
				}));

			setMessages(mappedMessages);
		} catch (err) {
			console.error("[UI] Failed to load chat history:", err);
		}
	};

	const checkIsAtBottom = () => {
		const container = scrollRef.current;
		if (!container) return true;
		const threshold = 50;
		const position =
			container.scrollHeight - container.scrollTop - container.clientHeight;
		return position < threshold;
	};

	const handleScroll = () => {
		isAtBottomRef.current = checkIsAtBottom();
	};

	useEffect(() => {
		textareaRef.current?.focus();
		if (scrollRef.current) {
			scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
		}
	}, []);

	useEffect(() => {
		if (!isLoading) {
			textareaRef.current?.focus();
		}
	}, [isLoading]);

	useEffect(() => {
		if (isAtBottomRef.current && scrollRef.current) {
			scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
		}
	}, [messages]);

	const handleSendMessage = async () => {
		if (!inputValue.trim() || isLoading || !selectedModel) return;

		const [providerId, modelId] = selectedModel;
		const messageContent = inputValue.trim();
		setInputValue("");
		setIsLoading(true);
		setPendingMessageId(Date.now().toString());

		const api = window.api;
		if (api) {
			try {
				await api.sendMessage(messageContent, modelId, providerId);
				console.log("[UI] sendMessage called successfully");
				// Reload chat history from Rust (source of truth) to show user message
				await loadChatHistory();
			} catch (err) {
				console.error("[UI] sendMessage failed:", err);
				setIsLoading(false);
				setPendingMessageId(null);
			}
		}
	};

	const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
		if (e.key === "Enter" && !e.shiftKey) {
			e.preventDefault();
			handleSendMessage();
		}
	};

	const clearChat = async () => {
		setMessages([]);
		const api = window.api;
		if (api) {
			try {
				await api.newSession();
			} catch (err) {
				console.error("[UI] newSession failed:", err);
			}
		}
	};

	const selectedModelData = selectedModel
		? models.find(
				(m) => m.providerId === selectedModel[0] && m.id === selectedModel[1],
			)
		: null;

	const selectedModelName = selectedModelData?.name || "Select a model";
	const selectedProviderName = selectedModelData?.providerId || "";
	const selectedModelKey = selectedModel
		? serializeModelKey(selectedModel[0], selectedModel[1])
		: "";

	const modelsByProvider = models.reduce(
		(acc, model) => {
			if (!acc[model.providerId]) {
				acc[model.providerId] = [];
			}
			acc[model.providerId].push(model);
			return acc;
		},
		{} as Record<string, Model[]>,
	);

	const filteredProviders = Object.entries(modelsByProvider)
		.map(([providerId, providerModels]) => ({
			providerId,
			models: providerModels.filter(
				(m) =>
					m.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
					m.providerId.toLowerCase().includes(searchQuery.toLowerCase()),
			),
		}))
		.filter((group) => group.models.length > 0);

	const openSelector = () => {
		setIsSelectorOpen(true);
		setTimeout(() => {
			selectTriggerRef.current?.click();
		}, 0);
	};

	return (
		<div className="flex h-screen flex-col bg-background">
			<header className="flex items-center justify-between border-b px-4 py-3">
				<div className="flex items-center gap-2">
					<Bot className="h-6 w-6" />
					<h1 className="text-lg font-semibold">Agent Chat</h1>
				</div>
				<Button
					variant="ghost"
					size="icon"
					onClick={clearChat}
					title="Clear chat"
				>
					<Trash2 className="h-4 w-4" />
				</Button>
			</header>

			<div
				className="flex-1 overflow-y-auto px-4"
				ref={scrollRef}
				onScroll={handleScroll}
			>
				<div className="mx-auto max-w-3xl py-4">
					{messages.length === 0 ? (
						<div className="flex h-full flex-col items-center justify-center text-muted-foreground">
							<Bot className="mb-4 h-12 w-12 opacity-50" />
							<p>Start a conversation by typing a message below.</p>
						</div>
					) : (
						messages.map((message) => (
							<ChatMessage
								key={message.id}
								content={message.content}
								role={message.role}
							/>
						))
					)}
					{isLoading && (
						<div className="flex items-center gap-2 text-muted-foreground">
							<Loader2 className="h-4 w-4 animate-spin" />
							<span className="text-sm">Thinking...</span>
						</div>
					)}
				</div>
			</div>

			<Separator />

			<div className="border-t bg-background p-4">
				<div className="mx-auto max-w-3xl">
					<div className="relative rounded-lg border bg-background overflow-hidden">
						<Textarea
							ref={textareaRef}
							value={inputValue}
							onChange={(e) => setInputValue(e.target.value)}
							onKeyDown={handleKeyDown}
							placeholder="Type a message..."
							className="min-h-[80px] resize-none border-0 pb-14 pt-3 focus-visible:ring-0 focus-visible:ring-offset-0"
							rows={3}
							disabled={isLoading}
						/>

						<div className="absolute bottom-2 left-2 z-10">
							{selectedModelData ? (
								<button
									type="button"
									onClick={openSelector}
									className="flex items-center gap-2 rounded-md px-2 py-1 text-sm transition-colors hover:bg-accent"
								>
									<span className="font-medium">{selectedModelName}</span>
									{selectedProviderName && (
										<>
											<span className="text-muted-foreground">•</span>
											<span className="text-muted-foreground text-xs">
												{selectedProviderName}
											</span>
										</>
									)}
								</button>
							) : (
								<button
									type="button"
									onClick={openSelector}
									className="rounded-md px-2 py-1 text-sm text-muted-foreground transition-colors hover:bg-accent"
								>
									Select a model
								</button>
							)}

							<Select
								value={selectedModelKey}
								onValueChange={(value) => {
									const [providerId, modelId] = deserializeModelKey(value);
									setSelectedModel([providerId, modelId]);
									setSearchQuery("");
								}}
								open={isSelectorOpen}
								onOpenChange={setIsSelectorOpen}
							>
								<SelectTrigger ref={selectTriggerRef} className="sr-only">
									<span />
								</SelectTrigger>
								<SelectContent
									position="popper"
									side="top"
									className="max-h-[400px] w-[320px]"
								>
									<div className="sticky top-0 z-10 border-b bg-popover px-3 py-2">
										<div className="relative">
											<Search className="absolute left-2 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
											<input
												type="text"
												placeholder="Search models..."
												value={searchQuery}
												onChange={(e) => setSearchQuery(e.target.value)}
												className="h-8 w-full rounded-md border border-input bg-transparent pl-8 pr-3 text-sm outline-none placeholder:text-muted-foreground focus-visible:ring-1 focus-visible:ring-ring"
												onClick={(e) => e.stopPropagation()}
											/>
										</div>
									</div>
									<div className="max-h-[320px] overflow-y-auto">
										{filteredProviders.length === 0 ? (
											<div className="px-3 py-4 text-center text-sm text-muted-foreground">
												No models found
											</div>
										) : (
											filteredProviders.map(
												({ providerId, models: providerModels }) => (
													<SelectGroup key={providerId}>
														<SelectLabel className="px-3 py-1.5 text-xs font-semibold text-muted-foreground">
															{providerId}
														</SelectLabel>
														{providerModels.map((model) => (
															<SelectItem
																key={serializeModelKey(
																	model.providerId,
																	model.id,
																)}
																value={serializeModelKey(
																	model.providerId,
																	model.id,
																)}
																className="pl-6"
															>
																{model.name}
															</SelectItem>
														))}
													</SelectGroup>
												),
											)
										)}
									</div>
								</SelectContent>
							</Select>
						</div>

						<div className="absolute bottom-2 right-2 z-10">
							<Button
								onClick={handleSendMessage}
								disabled={!inputValue.trim() || isLoading || !selectedModel}
								size="icon"
								className="h-9 w-9"
							>
								{isLoading ? (
									<Loader2 className="h-4 w-4 animate-spin" />
								) : (
									<Send className="h-4 w-4" />
								)}
							</Button>
						</div>
					</div>
				</div>
			</div>
		</div>
	);
}

export default App;
