import ReactMarkdown from "react-markdown";
import rehypeKatex from "rehype-katex";
import remarkMath from "remark-math";
import "katex/dist/katex.min.css";

interface ChatMessageProps {
	content: string;
	role: "user" | "assistant";
	isStreaming?: boolean;
}

export function ChatMessage({ content, role, isStreaming }: ChatMessageProps) {
	const isUser = role === "user";

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