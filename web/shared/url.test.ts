import assert from 'node:assert/strict';
import { test } from 'node:test';
import { isSafeHref } from './url.ts';

test('only plain http(s) URLs are safe', () => {
	assert.equal(isSafeHref('https://example.com/a?b=c'), true);
	assert.equal(isSafeHref('HTTP://example.com'), true);
	assert.equal(isSafeHref('javascript:alert(1)'), false);
	assert.equal(isSafeHref('data:text/html,x'), false);
	assert.equal(isSafeHref('/relative'), false);
	assert.equal(isSafeHref('https://a.com/"onclick='), false);
	assert.equal(isSafeHref('https://a.com/ b'), false);
});
