import assert from 'node:assert/strict';
import { test } from 'node:test';
import { parseSseBlock, SseSplitter } from './sse.ts';

test('a block parses into its event and data', () => {
	assert.deepEqual(parseSseBlock('event: turn_delta\ndata: {"a":1}\n'), {
		event: 'turn_delta',
		data: '{"a":1}'
	});
});

test('multi-line data is joined with newlines', () => {
	assert.deepEqual(parseSseBlock('event: x\ndata: a\ndata: b'), { event: 'x', data: 'a\nb' });
});

test('comments, garbage and incomplete blocks are null', () => {
	assert.equal(parseSseBlock(': working'), null);
	assert.equal(parseSseBlock('garbage'), null);
	assert.equal(parseSseBlock('event: x'), null);
	assert.equal(parseSseBlock('data: x'), null);
});

test('a message split across chunks is delivered once whole', () => {
	const splitter = new SseSplitter();
	assert.deepEqual(splitter.push('event: a\ndata: {"x":'), []);
	assert.deepEqual(splitter.push('1}\n\nevent: b\ndata: {}\n\n'), [
		{ event: 'a', data: '{"x":1}' },
		{ event: 'b', data: '{}' }
	]);
});

test('keep-alive comment blocks are dropped between messages', () => {
	const splitter = new SseSplitter();
	assert.deepEqual(splitter.push(': working\n\nevent: b\ndata: {}\n\n'), [{ event: 'b', data: '{}' }]);
});

test('CRLF framing is normalised', () => {
	const splitter = new SseSplitter();
	assert.deepEqual(splitter.push('event: a\r\ndata: 1\r\n\r\n'), [{ event: 'a', data: '1' }]);
});
