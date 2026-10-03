import assert from 'node:assert/strict';
import { test } from 'node:test';
import { agentThemeCss } from './theme.ts';

test('the rule sets the primary colour and its text colour, and nothing for a non-colour', () => {
	assert.equal(
		agentThemeCss('#FFD400'),
		':host:host:host:host{--color-primary:#ffd400;--color-primary-content:#1d1d1b}'
	);
	for (const bad of ['', null, undefined, 'red', '#fff', 'url(x)']) assert.equal(agentThemeCss(bad), '');
});
