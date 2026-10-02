/**
 * Parser for the `chat_json` SSE frames of `GET /api/v0/embed/events`.
 *
 * The widget reads them through `fetch` streaming, not `EventSource`, because
 * `EventSource` cannot send an `Authorization` header. Comment lines (`: working`
 * keep-alives) carry nothing and are dropped.
 */

import { SseSplitter } from '../shared/sse.ts';

export interface Frame {
	event: string;
	data: Record<string, unknown>;
}

/** Splits a byte stream into frames; feed it decoded text chunks as they arrive. */
export class FrameParser {
	private readonly splitter = new SseSplitter();

	push(chunk: string): Frame[] {
		const frames: Frame[] = [];
		for (const { event, data } of this.splitter.push(chunk)) {
			const parsed = parseObject(data);
			if (parsed) frames.push({ event, data: parsed });
		}
		return frames;
	}
}

function parseObject(json: string): Record<string, unknown> | null {
	try {
		const parsed: unknown = JSON.parse(json);
		if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) return null;
		return parsed as Record<string, unknown>;
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
