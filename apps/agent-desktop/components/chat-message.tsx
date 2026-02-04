import { Avatar, AvatarFallback } from "@/components/ui/avatar";

interface ChatMessageProps {
	content: string;
	role: "user" | "assistant";
}

export function ChatMessage({ content, role }: ChatMessageProps) {
	const isUser = role === "user";

	return (
		<div
			className={`flex gap-3 ${isUser ? "flex-row-reverse" : ""} mb-4`}
		>
			<Avatar className="h-8 w-8 shrink-0">
				<AvatarFallback className={isUser ? "bg-primary text-primary-foreground" : "bg-secondary"}>
					{isUser ? "U" : "AI"}
				</AvatarFallback>
			</Avatar>
			<div
				className={`max-w-[80%] rounded-lg px-4 py-2 ${
					isUser
						? "bg-primary text-primary-foreground ml-auto"
						: "bg-muted"
				}`}
			>
				<p className="text-sm whitespace-pre-wrap">{content}</p>
			</div>
		</div>
	);
}
