/**
 * Parser for the `chat_json` SSE frames of `GET /api/v0/embed/events`.
 *
 * The widget reads them through `fetch` streaming, not `EventSource`, because
 * `EventSource` cannot send an `Authorization` header. Comment lines (`: working`
 * keep-alives) carry nothing and are dropped.
 */

export interface Frame {
	event: string;
	data: Record<string, unknown>;
}

/** Splits a byte stream into frames; feed it decoded text chunks as they arrive. */
export class FrameParser {
	private buffer = '';

	push(chunk: string): Frame[] {
		this.buffer += chunk.replaceAll('\r\n', '\n');
		const blocks = this.buffer.split('\n\n');
		this.buffer = blocks.pop() ?? '';
		const frames: Frame[] = [];
		for (const block of blocks) {
			const frame = parseBlock(block);
			if (frame) frames.push(frame);
		}
		return frames;
	}
}

function parseBlock(block: string): Frame | null {
	let event: string | null = null;
	const data: string[] = [];
	for (const line of block.split('\n')) {
		if (line.startsWith('event:')) event = line.slice(6).trim();
		else if (line.startsWith('data:')) data.push(line.slice(5).replace(/^ /, ''));
	}
	if (event === null || data.length === 0) return null;
	try {
		const parsed: unknown = JSON.parse(data.join('\n'));
		if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) return null;
		return { event, data: parsed as Record<string, unknown> };
	} catch {
		return null;
	}
}

/** Every frame of a response body, as they arrive. */
export async function* readFrames(body: ReadableStream<Uint8Array>): AsyncGenerator<Frame> {
	const reader = body.getReader();
	const decoder = new TextDecoder();
	const parser = new FrameParser();
	try {
		for (;;) {
			const { done, value } = await reader.read();
			if (done) break;
			yield* parser.push(decoder.decode(value, { stream: true }));
		}
	} finally {
		reader.releaseLock();
	}
}
