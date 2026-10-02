import assert from 'node:assert/strict';
import { test } from 'node:test';
import { FrameParser, readFrames } from './frames.ts';

test('a frame split across chunks is delivered once whole', () => {
	const parser = new FrameParser();
	assert.deepEqual(parser.push('event: turn_delta\ndata: {"turn_id":"t1",'), []);
	const frames = parser.push('"text_delta":"Hi","full":true}\n\nevent: idle\ndata: {"type":"idle"}\n\n');
	assert.equal(frames.length, 2);
	assert.equal(frames[0].event, 'turn_delta');
	assert.equal(frames[0].data.text_delta, 'Hi');
	assert.equal(frames[1].event, 'idle');
});

test('keep-alive comments and malformed frames are dropped', () => {
	const parser = new FrameParser();
	const frames = parser.push(
		': working\n\nevent: x\ndata: not json\n\nevent: y\ndata: [1]\n\nevent: idle\ndata: {}\n\n'
	);
	assert.deepEqual(frames, [{ event: 'idle', data: {} }]);
});

test('CRLF framing and newlines inside JSON strings survive', () => {
	const parser = new FrameParser();
	const frames = parser.push('event: turn_delta\r\ndata: {"text_delta":"a\\nb"}\r\n\r\n');
	assert.equal(frames[0].data.text_delta, 'a\nb');
});

test('readFrames decodes a multi-byte character split across chunks', async () => {
	const bytes = new TextEncoder().encode('event: turn_delta\ndata: {"text_delta":"ß"}\n\n');
	const cut = bytes.indexOf(0xc3) + 1;
	const body = new ReadableStream<Uint8Array>({
		start(controller) {
			controller.enqueue(bytes.slice(0, cut));
			controller.enqueue(bytes.slice(cut));
			controller.close();
		}
	});
	const seen = [];
	for await (const frame of readFrames(body)) seen.push(frame.data.text_delta);
	assert.deepEqual(seen, ['ß']);
});
