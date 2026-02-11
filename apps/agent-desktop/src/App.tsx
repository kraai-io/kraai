import { Bot, Loader2, Send, Trash2 } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { ChatMessage } from "@/components/chat-message";
import { Button } from "@/components/ui/button";
import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from "@/components/ui/select";
import { Separator } from "@/components/ui/separator";
import { Textarea } from "@/components/ui/textarea";
import type { Event, Model as BindingModel } from "agent-ts-bindings";

interface Message {
	id: string;
	content: string;
	role: "user" | "assistant";
	timestamp: Date;
}

// Extended model type with provider info
interface Model extends BindingModel {
	providerId: string;
}

// Serialize/deserialize model key for Select component
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
	const [selectedModel, setSelectedModel] = useState<[string, string] | null>(null);
	const [isLoadingModels, setIsLoadingModels] = useState(true);
	const [pendingMessageId, setPendingMessageId] = useState<string | null>(null);
	const pendingMessageIdRef = useRef<string | null>(null);
	const scrollRef = useRef<HTMLDivElement>(null);
	const textareaRef = useRef<HTMLTextAreaElement>(null);
	const isAtBottomRef = useRef(true);

	// Keep ref in sync with state
	useEffect(() => {
		pendingMessageIdRef.current = pendingMessageId;
	}, [pendingMessageId]);

	const isInitializedRef = useRef(false);

	// Initialize runtime and set up event handler
	useEffect(() => {
		const api = (window as any).api;
		if (!api) return;
		if (isInitializedRef.current) return;
		isInitializedRef.current = true;

		// Initialize runtime with event handler
		api.initRuntime((event: Event) => {
			console.log("[UI] Received event from Rust:", event);

			if (event.type === "ConfigLoaded") {
				console.log("[UI] Config loaded");
				// Refresh models when config is loaded
				loadModels();
			} else if (event.type === "Error") {
				console.error("[UI] Error:", event.field0);
				setIsLoading(false);
			} else if (event.type === "MessageComplete") {
				console.log("[UI] Message complete:", event.field0);
				if (event.field0 && pendingMessageIdRef.current) {
					const assistantMessage: Message = {
						id: (Date.now() + 1).toString(),
						content: event.field0,
						role: "assistant",
						timestamp: new Date(),
					};
					setMessages((prev) => [...prev, assistantMessage]);
					setIsLoading(false);
					setPendingMessageId(null);
				}
			} else {
				console.log("[UI] Unknown event type:", (event as any).type);
			}
		});

		// Load models initially
		loadModels();
	}, []);

	const loadModels = async () => {
		const api = (window as any).api;
		if (!api) return;

		try {
			// Get models as HashMap from Rust
			const modelMap: Record<string, Array<BindingModel>> =
				await api.listModels();
			console.log("[UI] Models loaded:", modelMap);

			// Flatten the map into array with providerId included
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
			setIsLoadingModels(false);
		} catch (err) {
			console.error("[UI] Failed to load models:", err);
			setIsLoadingModels(false);
		}
	};

	// Check if scroll is at bottom
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

		const userMessage: Message = {
			id: Date.now().toString(),
			content: inputValue.trim(),
			role: "user",
			timestamp: new Date(),
		};

		setMessages((prev) => [...prev, userMessage]);
		setInputValue("");
		setIsLoading(true);
		setPendingMessageId(userMessage.id);

		const api = (window as any).api;
		if (api) {
			try {
				await api.sendMessage(userMessage.content, modelId, providerId);
				console.log("[UI] sendMessage called successfully");
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

	const clearChat = () => {
		setMessages([]);
	};

	const selectedModelName = selectedModel
		? models.find(
				(m) =>
					m.providerId === selectedModel[0] && m.id === selectedModel[1],
			)?.name || "Unknown model"
		: "Select a model";

	const selectedModelKey = selectedModel
		? serializeModelKey(selectedModel[0], selectedModel[1])
		: "";

	return (
		<div className="flex h-screen flex-col bg-background">
			{/* Header */}
			<header className="flex items-center justify-between border-b px-4 py-3">
				<div className="flex items-center gap-2">
					<Bot className="h-6 w-6" />
					<h1 className="text-lg font-semibold">Agent Chat</h1>
					<span className="rounded-full bg-muted px-2 py-0.5 text-xs text-muted-foreground">
						Connected
					</span>
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

			{/* Messages Area */}
			<div
				className="flex-1 overflow-y-auto px-4 scrollbar-themed"
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

			{/* Input Area */}
			<div className="border-t bg-background p-4">
				<div className="mx-auto flex max-w-3xl items-end gap-2">
					{/* Model Selector */}
					<Select
						disabled={isLoadingModels || models.length === 0}
						value={selectedModelKey}
						onValueChange={(value) => {
							const [providerId, modelId] = deserializeModelKey(value);
							setSelectedModel([providerId, modelId]);
						}}
					>
						<SelectTrigger className="w-[180px] shrink-0">
							<SelectValue placeholder="Select a model">
								{selectedModelName}
							</SelectValue>
						</SelectTrigger>
						<SelectContent>
							{models.map((model) => (
								<SelectItem
									key={serializeModelKey(model.providerId, model.id)}
									value={serializeModelKey(model.providerId, model.id)}
								>
									{model.name}
								</SelectItem>
							))}
						</SelectContent>
					</Select>
					<Textarea
						ref={textareaRef}
						value={inputValue}
						onChange={(e) => setInputValue(e.target.value)}
						onKeyDown={handleKeyDown}
						placeholder="Type a message... (Shift+Enter for new line)"
						className="min-h-[60px] resize-none"
						rows={1}
						disabled={isLoading}
					/>
					<Button
						onClick={handleSendMessage}
						disabled={!inputValue.trim() || isLoading || !selectedModel}
						size="icon"
						className="h-[60px] w-[60px] shrink-0"
					>
						{isLoading ? (
							<Loader2 className="h-4 w-4 animate-spin" />
						) : (
							<Send className="h-4 w-4" />
						)}
					</Button>
				</div>
				<p className="mx-auto mt-2 max-w-3xl text-center text-xs text-muted-foreground">
					Press Enter to send, Shift+Enter for new line
				</p>
			</div>
		</div>
	);
}

export default App;
