import { useState, useRef, useEffect } from "react";
import { Send, Loader2, Bot, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";
import { ChatMessage } from "@/components/chat-message";

interface Message {
	id: string;
	content: string;
	role: "user" | "assistant";
	timestamp: Date;
}

// Mock responses for the toy UI
const MOCK_RESPONSES = [
	"I'm a mock AI assistant. The Rust backend isn't connected yet, but this UI is ready for when it is!",
	"This is a placeholder response. Once the Rust code is fixed, real AI responses will appear here.",
	"The chat interface is working! You can type messages and see them appear in the chat.",
	"Hello! I'm running in demo mode. The TypeScript frontend is ready for LLM integration.",
	"Thanks for your message! This is a test response while the backend is being developed.",
];

function generateMockResponse(userMessage: string): string {
	// Simple pattern matching for variety
	if (userMessage.toLowerCase().includes("hello") || userMessage.toLowerCase().includes("hi")) {
		return "Hello there! Welcome to the Agent Chat demo. The UI is ready for real LLM integration.";
	}
	if (userMessage.toLowerCase().includes("help")) {
		return "I can help demonstrate the chat interface! Type any message and I'll respond with a placeholder message.";
	}
	if (userMessage.toLowerCase().includes("rust")) {
		return "The Rust backend is still being worked on. Once it's ready, this UI will connect to real LLM providers!";
	}
	// Random response
	const randomIndex = Math.floor(Math.random() * MOCK_RESPONSES.length);
	return MOCK_RESPONSES[randomIndex];
}

function App(): React.JSX.Element {
	const [messages, setMessages] = useState<Message[]>([
		{
			id: "welcome",
			content: "Hello! I'm your AI assistant. This is a toy UI for testing - real LLM integration coming soon!",
			role: "assistant",
			timestamp: new Date(),
		},
	]);
	const [inputValue, setInputValue] = useState("");
	const [isLoading, setIsLoading] = useState(false);
	const scrollRef = useRef<HTMLDivElement>(null);
	const textareaRef = useRef<HTMLTextAreaElement>(null);

	// Auto-scroll to bottom when messages change
	useEffect(() => {
		if (scrollRef.current) {
			scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
		}
	}, [messages]);

	// Autofocus textarea on mount
	useEffect(() => {
		textareaRef.current?.focus();
	}, []);

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

		// Simulate network delay
		await new Promise((resolve) => setTimeout(resolve, 1000));

		const assistantMessage: Message = {
			id: (Date.now() + 1).toString(),
			content: generateMockResponse(userMessage.content),
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
						Demo Mode
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
			<ScrollArea className="flex-1 px-4" ref={scrollRef}>
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
			</ScrollArea>

			<Separator />

			{/* Input Area */}
			<div className="border-t bg-background p-4">
				<div className="mx-auto flex max-w-3xl items-end gap-2">
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
					Press Enter to send, Shift+Enter for new line • Demo mode with mock responses
				</p>
			</div>
		</div>
	);
}

export default App;
