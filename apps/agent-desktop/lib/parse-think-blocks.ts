export interface ContentSegment {
	type: "thinking" | "content";
	content: string;
	isStreaming?: boolean;
}

export function parseThinkBlocks(
	content: string,
	isCurrentlyStreaming: boolean,
): ContentSegment[] {
	const segments: ContentSegment[] = [];
	let remaining = content;

	// Regex to find opening tags
	const openTagRegex = /<(think|thinking)([^>]*)>/gi;
	// Regex to find closing tags (handles variations like </think<?>>)
	const closeTagRegex = /<\/(think|thinking)[^>]*>/gi;

	// Process content looking for think blocks
	while (remaining.length > 0) {
		openTagRegex.lastIndex = 0;
		const openMatch = openTagRegex.exec(remaining);

		if (!openMatch) {
			// No more think tags, add remaining as content
			if (remaining.trim()) {
				segments.push({ type: "content", content: remaining });
			}
			break;
		}

		// Add content before the think tag
		if (openMatch.index > 0) {
			const beforeContent = remaining.slice(0, openMatch.index);
			if (beforeContent.trim()) {
				segments.push({ type: "content", content: beforeContent });
			}
		}

		const afterOpen = remaining.slice(openMatch.index + openMatch[0].length);

		// Find the closing tag
		closeTagRegex.lastIndex = 0;
		const closeMatch = closeTagRegex.exec(afterOpen);

		if (closeMatch) {
			// Complete think block found
			const thinkingContent = afterOpen.slice(0, closeMatch.index);
			segments.push({
				type: "thinking",
				content: thinkingContent,
				isStreaming: false,
			});
			remaining = afterOpen.slice(closeMatch.index + closeMatch[0].length);
		} else if (isCurrentlyStreaming) {
			// Incomplete think block during streaming
			segments.push({
				type: "thinking",
				content: afterOpen,
				isStreaming: true,
			});
			remaining = "";
		} else {
			// No close tag and not streaming - treat as content
			segments.push({ type: "content", content: remaining });
			break;
		}
	}

	return segments;
}
