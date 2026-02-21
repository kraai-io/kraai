import { Wrench } from "lucide-react";
import ReactMarkdown from "react-markdown";
import rehypeKatex from "rehype-katex";
import remarkMath from "remark-math";
import "katex/dist/katex.min.css";

interface ChatMessageProps {
	content: string;
	role: "user" | "assistant" | "tool";
	isStreaming?: boolean;
}

export function ChatMessage({ content, role, isStreaming }: ChatMessageProps) {
	const isUser = role === "user";
	const isTool = role === "tool";

	if (isTool) {
		return (
			<div className="py-2">
				<div className="bg-muted/50 border rounded-lg px-4 py-2.5">
					<div className="flex items-center gap-2 text-muted-foreground text-sm mb-1">
						<Wrench className="h-4 w-4" />
						<span>Tool Result</span>
					</div>
					<pre className="text-sm overflow-x-auto whitespace-pre-wrap break-all">
						{content}
					</pre>
					{isStreaming && (
						<span className="inline-block ml-1 h-4 w-1 animate-pulse bg-current opacity-60" />
					)}
				</div>
			</div>
		);
	}

	return (
		<div className="py-2">
			{isUser ? (
				<div className="bg-slate-800 px-4 py-2.5 text-white">
					<p className="text-base whitespace-pre-wrap">{content}</p>
					{isStreaming && (
						<span className="inline-block ml-1 h-4 w-1 animate-pulse bg-current opacity-60" />
					)}
				</div>
			) : (
				<div className="text-base prose prose-sm max-w-none dark:prose-invert">
					<ReactMarkdown
						remarkPlugins={[remarkMath]}
						rehypePlugins={[rehypeKatex]}
					>
						{content}
					</ReactMarkdown>
				</div>
			)}
		</div>
	);
}
