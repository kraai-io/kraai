import * as Collapsible from "@radix-ui/react-collapsible";
import { Brain, ChevronRight } from "lucide-react";
import { useState } from "react";

interface ThinkingBlockProps {
	content: string;
	isStreaming?: boolean;
}

export function ThinkingBlock({
	content,
	isStreaming = false,
}: ThinkingBlockProps) {
	const [isOpen, setIsOpen] = useState(false);

	return (
		<Collapsible.Root open={isOpen} onOpenChange={setIsOpen}>
			<Collapsible.Trigger className="flex items-center gap-2 py-2 text-sm text-muted-foreground hover:text-foreground transition-colors cursor-pointer group">
				<ChevronRight
					className={`h-4 w-4 transition-transform duration-200 ${isOpen ? "rotate-90" : ""}`}
				/>
				<Brain className={`h-4 w-4 ${isStreaming ? "animate-pulse" : ""}`} />
				<span className="font-medium">
					{isStreaming ? "Thinking..." : "Thinking"}
				</span>
			</Collapsible.Trigger>
			<Collapsible.Content className="overflow-hidden">
				<div className="pl-6 py-2 text-sm text-muted-foreground border-l-2 border-muted ml-2 mt-1 whitespace-pre-wrap">
					{content}
				</div>
			</Collapsible.Content>
		</Collapsible.Root>
	);
}
