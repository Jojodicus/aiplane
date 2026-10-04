import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const conversation = readFileSync(new URL('./Conversation.svelte', import.meta.url), 'utf8');
const shell = readFileSync(new URL('../routes/+layout.svelte', import.meta.url), 'utf8');
const agentShell = readFileSync(new URL('./components/agents/AgentShell.svelte', import.meta.url), 'utf8');
const workbench = readFileSync(new URL('./components/agents/AgentWorkbench.svelte', import.meta.url), 'utf8');
const testChat = readFileSync(new URL('./components/agents/TestChat.svelte', import.meta.url), 'utf8');
const streamed = readFileSync(new URL('./components/chat/StreamedChat.svelte', import.meta.url), 'utf8');

test('the application shell gives chat its own bounded viewport', () => {
	assert.match(shell, /h-dvh overflow-clip/);
	assert.match(shell, /boundedViewport\(page\.url, base\)/);
	assert.match(shell, /bounded \? 'overflow-hidden'/);
	assert.match(shell, /bounded \? 'h-full/);
});

test('the transcript scrolls independently while the full-width composer stays outside it', () => {
	const transcript = conversation.indexOf('data-chat-transcript');
	const canvas = conversation.indexOf('<ConversationCanvas');
	const composer = conversation.indexOf('data-chat-composer');

	assert.ok(transcript >= 0);
	assert.ok(canvas > transcript);
	assert.ok(composer > canvas);
	assert.match(conversation, /data-chat-transcript[\s\S]*?overflow-y-auto/);
	assert.match(conversation, /data-chat-composer[\s\S]*?w-full/);
});

test('the application shell clips rather than hides, so focusing a field cannot scroll it sideways', () => {
	assert.doesNotMatch(shell, /flex h-dvh overflow-hidden/);
});

test('the test chat fills the bounded viewport down a flex column, with no guessed offsets', () => {
	assert.match(agentShell, /data-agent-page class="flex w-full flex-col gap-4 \{bounded \? 'h-full min-h-0'/);
	assert.match(workbench, /data-agent-tab class=\{sub === 'test' \? 'flex min-h-0 flex-1 flex-col'/);
	assert.match(testChat, /data-test-chat class="flex min-h-0 flex-1 flex-col/);
	assert.match(testChat, /grid min-h-0 flex-1/);
	assert.doesNotMatch(testChat, /100dvh/);
	assert.match(streamed, /flex min-h-0 flex-1 flex-col/);
	assert.match(streamed, /data-chat-transcript[^>]*min-h-0 flex-1 overflow-y-auto/);
	assert.match(streamed, /data-chat-composer class="flex w-full shrink-0/);
	assert.ok(streamed.indexOf('data-chat-composer') > streamed.indexOf('data-chat-transcript'));
});
