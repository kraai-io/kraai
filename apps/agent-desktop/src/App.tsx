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

interface Message {
	id: string;
	content: string;
	role: "user" | "assistant";
	timestamp: Date;
}

interface Event {
	eventType: string;
	data?: string;
}

function App(): React.JSX.Element {
	const [messages, setMessages] = useState<Message[]>([
		{
			id: "welcome",
			content: "Hello! The Rust runtime is connected.",
			role: "assistant",
			timestamp: new Date(),
		},
	]);
	const [inputValue, setInputValue] = useState("");
	const [isLoading, setIsLoading] = useState(false);
	const [models, setModels] = useState<string[]>([]);
	const [selectedModel, setSelectedModel] = useState<string | null>(null);
	const [isLoadingModels, setIsLoadingModels] = useState(true);
	const scrollRef = useRef<HTMLDivElement>(null);
	const textareaRef = useRef<HTMLTextAreaElement>(null);
	const isAtBottomRef = useRef(true);

	// Initialize runtime and set up event handler
	useEffect(() => {
		const api = (window as any).api;
		if (!api) return;

		// Initialize runtime with event handler
		api.initRuntime((event: Event) => {
			console.log("[UI] Received event from Rust:", event);

			switch (event.eventType) {
				case "config_loaded":
					console.log("[UI] Config loaded:", event.data);
					// Refresh models when config is loaded
					api.listModels().then((modelList: string[]) => {
						setModels(modelList);
						if (modelList.length > 0 && !selectedModel) {
							setSelectedModel(modelList[0]);
						}
					});
					break;
				case "config_error":
					console.error("[UI] Config error:", event.data);
					break;
				case "config_reloaded":
					console.log("[UI] Config reloaded:", event.data);
					// Refresh models when config is reloaded
					api.listModels().then((modelList: string[]) => {
						setModels(modelList);
						if (modelList.length > 0) {
							setSelectedModel(modelList[0]);
						}
					});
					break;
				case "config_reload_error":
					console.error("[UI] Config reload error:", event.data);
					break;
				case "test":
					console.log("[UI] Test event data:", event.data);
					break;
				default:
					console.log("[UI] Unknown event type:", event.eventType);
			}
		});

		// Load models
		api
			.listModels()
			.then((modelList: string[]) => {
				console.log("[UI] Models loaded:", modelList);
				setModels(modelList);
				if (modelList.length > 0) {
					setSelectedModel(modelList[0]);
				}
				setIsLoadingModels(false);
			})
			.catch((err: any) => {
				console.error("[UI] Failed to load models:", err);
				setIsLoadingModels(false);
			});
	}, []);

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
		if (!inputValue.trim() || isLoading) return;

		const userMessage: Message = {
			id: Date.now().toString(),
			content: inputValue.trim(),
			role: "user",
			timestamp: new Date(),
		};

		setMessages((prev) => [...prev, userMessage]);
		setInputValue("");
		setIsLoading(true);

		// Test the Rust callback system
		const api = (window as any).api;
		if (api) {
			try {
				await api.doSomething();
				console.log("[UI] doSomething called successfully");
			} catch (err) {
				console.error("[UI] doSomething failed:", err);
			}
		}

		// Mock response for now
		await new Promise((resolve) => setTimeout(resolve, 1000));

		const assistantMessage: Message = {
			id: (Date.now() + 1).toString(),
			content: `Echo: ${userMessage.content}`,
			role: "assistant",
			timestamp: new Date(),
		};

		setMessages((prev) => [...prev, assistantMessage]);
		setIsLoading(false);
	};

	const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
		if (e.key === "Enter" && !e.shiftKey) {
			e.preventDefault();
			handleSendMessage();
		}
	};

	const clearChat = () => {
		setMessages([
			{
				id: "welcome",
				content: "Chat cleared! How can I help you today?",
				role: "assistant",
				timestamp: new Date(),
			},
		]);
	};

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
						value={selectedModel || ""}
						onValueChange={setSelectedModel}
					>
						<SelectTrigger className="w-[180px] shrink-0">
							<SelectValue placeholder="Select a model" />
						</SelectTrigger>
						<SelectContent>
							{models.map((model) => (
								<SelectItem key={model} value={model}>
									{model}
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
						disabled={!inputValue.trim() || isLoading}
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
