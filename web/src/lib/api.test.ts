import assert from 'node:assert/strict';
import test from 'node:test';

import { ApiError, request } from './api.ts';

test('successful empty responses complete without JSON parsing', async (context) => {
	const originalFetch = globalThis.fetch;
	context.after(() => { globalThis.fetch = originalFetch; });
	globalThis.fetch = async () => new Response(null, { status: 204 });
	assert.equal(await request<void>('/empty'), undefined);
});

test('a refusal carries the envelope: code, the server sentence and its issues', async (context) => {
	const originalFetch = globalThis.fetch;
	context.after(() => { globalThis.fetch = originalFetch; });
	const body = JSON.stringify({
		error: {
			message: 'cannot save the test case: at `expect`, empty',
			code: 'invalid_test_case',
			issues: [{ path: 'expect', message: 'empty' }]
		}
	});
	globalThis.fetch = async () => new Response(body, { status: 422 });

	const err = await request('/cases').catch((e: unknown) => e);

	assert.ok(err instanceof ApiError);
	assert.equal(err.status, 422);
	assert.equal(err.code, 'invalid_test_case');
	assert.equal(err.serverMessage, 'cannot save the test case: at `expect`, empty');
	assert.deepEqual(err.issues, [{ path: 'expect', message: 'empty' }]);
	assert.equal(err.detail, body);
});

test('a body that is no envelope leaves only the status and the raw text', async (context) => {
	const originalFetch = globalThis.fetch;
	context.after(() => { globalThis.fetch = originalFetch; });
	globalThis.fetch = async () => new Response('upstream timeout', { status: 502 });

	const err = await request('/x').catch((e: unknown) => e);

	assert.ok(err instanceof ApiError);
	assert.equal(err.code, undefined);
	assert.equal(err.serverMessage, undefined);
	assert.deepEqual(err.issues, []);
	assert.equal(err.detail, 'upstream timeout');
});
