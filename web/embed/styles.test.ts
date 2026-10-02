import assert from 'node:assert/strict';
import { test } from 'node:test';
import { forShadowRoot } from './styles.ts';

test(':root theme selectors move to :host', () => {
	const css = forShadowRoot(':root,[data-theme=light]{--color-primary:#6c3eb5}:root:not([data-theme]){--x:1}');
	assert.ok(css.includes(':host,[data-theme=light]{--color-primary:#6c3eb5}'));
	assert.ok(css.includes(':host:not([data-theme]){--x:1}'));
	assert.ok(!css.includes(':root'));
});

test('@property defaults become declarations a shadow tree honours', () => {
	const css = forShadowRoot(
		'@property --tw-shadow{syntax:"*";inherits:false;initial-value:0 0 #0000}.a{color:red}'
	);
	assert.ok(!css.includes('@property'));
	assert.ok(css.includes('*,:before,:after,::backdrop{--tw-shadow:0 0 #0000}'));
	assert.ok(css.includes('.a{color:red}'));
});

test('the host starts from a clean slate so page styles do not inherit in', () => {
	assert.ok(forShadowRoot('.a{}').startsWith(':host{all:initial}'));
});
