import { test } from 'node:test';
import assert from 'node:assert/strict';

import { browserSetupStage, chromeWebStoreUrl, EXTENSION_DOWNLOAD_PATH } from './browser-extension.ts';

const absent = { present: false, armed: false };
const paired = { present: true, armed: false };
const armed = { present: true, armed: true };

test('a role without the tool is told so before anything about the extension', () => {
	assert.equal(browserSetupStage(undefined, armed), 'not_granted');
});

test('a granted tool the user switched off says so even with the extension on', () => {
	assert.equal(browserSetupStage({ enabled: false }, armed), 'tool_off');
});

test('no answer from the extension means it is not installed or not paired with this origin', () => {
	assert.equal(browserSetupStage({ enabled: true }, absent), 'not_detected');
});

test('a paired extension that is switched off waits for the switch', () => {
	assert.equal(browserSetupStage({ enabled: true }, paired), 'switched_off');
});

test('a paired, armed extension with the tool on is ready', () => {
	assert.equal(browserSetupStage({ enabled: true }, armed), 'ready');
});

test('the store link points at the published item', () => {
	assert.match(chromeWebStoreUrl, /^https:\/\/chromewebstore\.google\.com\/detail\/[a-p]{32}$/);
});

test('the download is served from the SPA root, not a route the client router owns', () => {
	assert.equal(EXTENSION_DOWNLOAD_PATH, '/downloads/aiplane-browser-control.zip');
});
