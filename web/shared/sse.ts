/**
 * Server-sent-event block parsing shared by the SPA and the embed widget.
 * Pure TypeScript: no Svelte, no `$lib`, no DOM, so both bundles can import it
 * without pulling in the other's code.
 */

export interface SseMessage {
	event: string;
	data: string;
}

/** One SSE block (`event:` + `data:` lines) as a message; comments and incomplete blocks yield null. */
export function parseSseBlock(block: string): SseMessage | null {
	let event: string | null = null;
	const data: string[] = [];
	for (const line of block.split('\n')) {
		if (line.startsWith('event:')) event = line.slice(6).trim();
		else if (line.startsWith('data:')) data.push(line.slice(5).replace(/^ /, ''));
	}
	if (event === null || data.length === 0) return null;
	return { event, data: data.join('\n') };
}

/** Splits a text stream into SSE messages; feed it decoded chunks as they arrive. */
export class SseSplitter {
	private buffer = '';

	push(chunk: string): SseMessage[] {
		this.buffer += chunk.replaceAll('\r\n', '\n');
		const blocks = this.buffer.split('\n\n');
		this.buffer = blocks.pop() ?? '';
		const messages: SseMessage[] = [];
		for (const block of blocks) {
			const message = parseSseBlock(block);
			if (message) messages.push(message);
		}
		return messages;
	}
}
