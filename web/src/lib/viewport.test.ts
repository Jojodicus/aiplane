import assert from 'node:assert/strict';
import test from 'node:test';
import { boundedViewport } from './viewport.ts';

const at = (path: string) => new URL(`https://gw.example${path}`);

test('the chat and an agent test chat fill the window, every other page scrolls', () => {
	assert.ok(boundedViewport(at('/chat')));
	assert.ok(boundedViewport(at('/chat/s1')));
	assert.ok(boundedViewport(at('/agents/support?tab=try')), 'Test chat is the tab default');
	assert.ok(boundedViewport(at('/agents/support?tab=try&sub=test')));
	assert.ok(!boundedViewport(at('/agents/support?tab=try&sub=tests')));
	assert.ok(!boundedViewport(at('/agents/support')));
	assert.ok(!boundedViewport(at('/agents/support/setup?tab=try')));
	assert.ok(!boundedViewport(at('/agents')));
	assert.ok(!boundedViewport(at('/chatter')));
});
