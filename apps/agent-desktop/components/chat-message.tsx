import { User, Bot } from "lucide-react";

interface ChatMessageProps {
	content: string;
	role: "user" | "assistant";
	isStreaming?: boolean;
}

export function ChatMessage({ content, role, isStreaming }: ChatMessageProps) {
	const isUser = role === "user";

	return (
		<div className={`flex gap-3 ${isUser ? "flex-row-reverse" : ""} py-4 first:pt-8`}>
			<div
				className={`flex h-8 w-8 shrink-0 items-center justify-center rounded-lg ${
					isUser ? "bg-primary text-primary-foreground" : "bg-muted"
				}`}
			>
				{isUser ? <User className="h-4 w-4" /> : <Bot className="h-4 w-4" />}
			</div>
			<div className={`flex-1 ${isUser ? "text-right" : ""}`}>
				<div
					className={`inline-block max-w-[85%] rounded-2xl px-4 py-2.5 ${
						isUser
							? "bg-primary text-primary-foreground rounded-tr-md"
							: "bg-muted rounded-tl-md"
					}`}
				>
					<p className="text-sm whitespace-pre-wrap">{content}</p>
					{isStreaming && (
						<span className="inline-block ml-1 h-4 w-1 animate-pulse bg-current opacity-60" />
					)}
				</div>
			</div>
		</div>
	);
}