import assert from 'node:assert/strict';
import { test } from 'node:test';
import { contrastRatio, parseHex, readableText } from './color.ts';

test('only a #rrggbb colour paints the widget', () => {
	assert.deepEqual(parseHex('#0B6bcb'), [11, 107, 203]);
	for (const bad of ['', null, undefined, 'red', '#fff', '#12345g', 'url(x)']) {
		assert.equal(parseHex(bad), null, String(bad));
	}
});

test('the text on the agent colour is whichever of white and black reads better', () => {
	assert.equal(readableText(parseHex('#0b6bcb')!), '#ffffff');
	assert.equal(readableText(parseHex('#ffd400')!), '#1d1d1b');
	assert.equal(readableText(parseHex('#ffffff')!), '#1d1d1b');
	assert.equal(readableText(parseHex('#000000')!), '#ffffff');
	for (const color of ['#0b6bcb', '#ffd400', '#6c3eb5', '#22c55e', '#777777']) {
		const bg = parseHex(color)!;
		assert.ok(contrastRatio(bg, parseHex(readableText(bg))!) >= 4.4, color);
	}
});

