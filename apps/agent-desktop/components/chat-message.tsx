import { ChevronDown, ChevronRight, FileText, Wrench } from "lucide-react";
import { useState } from "react";
import ReactMarkdown from "react-markdown";
import rehypeKatex from "rehype-katex";
import remarkMath from "remark-math";
import "katex/dist/katex.min.css";
import { parseThinkBlocks } from "@/lib/parse-think-blocks";
import { ThinkingBlock } from "./thinking-block";

interface ToolResultDisplayProps {
	toolName: string;
	rawContent: string;
}

function ToolResultDisplay({ toolName, rawContent }: ToolResultDisplayProps) {
	try {
		const parsed = JSON.parse(rawContent);

		// Handle read_files tool output
		if (
			toolName === "read_files" &&
			parsed.files &&
			Array.isArray(parsed.files)
		) {
			return (
				<div className="mt-2 space-y-3">
					{parsed.files.map((fileContent: string, idx: number) => (
						<div
							key={`file-${idx}-${fileContent.slice(0, 50)}`}
							className="border rounded-lg overflow-hidden"
						>
							<div className="bg-muted/70 px-3 py-1.5 text-xs font-medium text-muted-foreground flex items-center gap-1.5">
								<FileText className="h-3 w-3" />
								File {idx + 1}
							</div>
							<pre className="text-xs p-3 overflow-x-auto bg-muted/30 max-h-96 overflow-y-auto">
								{fileContent}
							</pre>
						</div>
					))}
				</div>
			);
		}

		// Handle error output
		if (parsed.error) {
			return (
				<div className="mt-2 border border-destructive/50 rounded-lg p-3 text-xs bg-destructive/10">
					<span className="font-medium text-destructive">Error:</span>{" "}
					<span className="text-destructive/90">{parsed.error}</span>
				</div>
			);
		}

		// Generic JSON output with nice formatting
		return (
			<pre className="mt-2 text-xs bg-muted/30 border rounded-lg p-3 overflow-x-auto">
				{JSON.stringify(parsed, null, 2)}
			</pre>
		);
	} catch {
		// Not valid JSON, show as-is
		return (
			<pre className="mt-2 text-xs bg-muted/30 border rounded-lg p-3 overflow-x-auto whitespace-pre-wrap">
				{rawContent}
			</pre>
		);
	}
}

interface ChatMessageProps {
	content: string;
	role: "user" | "assistant" | "tool";
	isStreaming?: boolean;
}

export function ChatMessage({ content, role, isStreaming }: ChatMessageProps) {
	const [toolExpanded, setToolExpanded] = useState(false);
	const isUser = role === "user";
	const isTool = role === "tool";

	if (isTool) {
		// Parse tool name from content like "Tool 'tool_id' result:" or "Tool 'tool_id' was denied"
		const toolMatch = content.match(/Tool '([^']+)'/);
		const toolName = toolMatch ? toolMatch[1] : "unknown";
		const wasDenied = content.includes("denied by user");
		// Extract result content after "Tool 'tool_id' result:\n"
		const resultMatch = content.match(/Tool '[^']+' result:\n?([\s\S]*)/);
		const resultContent = resultMatch ? resultMatch[1] : null;

		return (
			<div className="py-1">
				<button
					type="button"
					onClick={() => setToolExpanded(!toolExpanded)}
					className="inline-flex items-center gap-1.5 rounded-full bg-muted px-2.5 py-1 text-xs text-muted-foreground hover:bg-muted/80 transition-colors cursor-pointer"
				>
					{toolExpanded ? (
						<ChevronDown className="h-3 w-3" />
					) : (
						<ChevronRight className="h-3 w-3" />
					)}
					<Wrench className="h-3 w-3" />
					<span className="font-medium">{toolName}</span>
					{wasDenied && <span className="text-destructive">(denied)</span>}
				</button>
				{toolExpanded && resultContent && (
					<ToolResultDisplay toolName={toolName} rawContent={resultContent} />
				)}
			</div>
		);
	}

	if (isUser) {
		return (
			<div className="py-2">
				<div className="bg-slate-800 px-4 py-2.5 text-white">
					<p className="text-base whitespace-pre-wrap">{content}</p>
					{isStreaming && (
						<span className="inline-block ml-1 h-4 w-1 animate-pulse bg-current opacity-60" />
					)}
				</div>
			</div>
		);
	}

	// Assistant message - parse think blocks
	const segments = parseThinkBlocks(content, isStreaming ?? false);

	return (
		<div className="py-2">
			{segments.map((segment, index) => {
				const key = `${segment.type}-${index}`;
				if (segment.type === "thinking") {
					return (
						<ThinkingBlock
							key={key}
							content={segment.content}
							isStreaming={segment.isStreaming}
						/>
					);
				}
				return (
					<div
						key={key}
						className="text-base prose prose-sm max-w-none dark:prose-invert"
					>
						<ReactMarkdown
							remarkPlugins={[remarkMath]}
							rehypePlugins={[rehypeKatex]}
						>
							{segment.content}
						</ReactMarkdown>
						{segment.isStreaming && (
							<span className="inline-block ml-1 h-4 w-1 animate-pulse bg-current opacity-60" />
						)}
					</div>
				);
			})}
			{isStreaming &&
				segments.length > 0 &&
				segments[segments.length - 1].type !== "thinking" &&
				!segments[segments.length - 1].isStreaming && (
					<span className="inline-block ml-1 h-4 w-1 animate-pulse bg-current opacity-60" />
				)}
		</div>
	);
}
