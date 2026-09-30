// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { generateKeyPairSync } from 'node:crypto';
import { writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

const script = new URL('./publish-extension.mjs', import.meta.url);

function run(args, env) {
	return spawnSync(process.execPath, [script.pathname, ...args], {
		encoding: 'utf8',
		env
	});
}

test('malformed credential JSON never exposes parser input', () => {
	const secretFragment = 'PRIVATE_KEY_FRAGMENT_MUST_NOT_LEAK';
	const result = run(['--check'], {
		CWS_SERVICE_ACCOUNT: `{"private_key":"${secretFragment}`
	});
	const output = `${result.stdout}${result.stderr}`;

	assert.equal(result.status, 1);
	assert.match(result.stderr, /CWS_SERVICE_ACCOUNT is not valid JSON/);
	assert.equal(output.includes(secretFragment), false);
});

test('missing variable diagnostics name variables without their values', () => {
	const result = run(['extension.zip'], {
		CWS_SERVICE_ACCOUNT: '{}'
	});

	assert.equal(result.status, 1);
	assert.equal(result.stderr, 'missing CWS_PUBLISHER_ID, CWS_EXTENSION_ID\n');
});

/** Async, unlike `run`: the fake store lives in this process and must answer. */
function runAsync(args, env) {
	return new Promise((resolve) => {
		const child = spawn(process.execPath, [script.pathname, ...args], { env });
		let stdout = '';
		let stderr = '';
		child.stdout.on('data', (chunk) => (stdout += chunk));
		child.stderr.on('data', (chunk) => (stderr += chunk));
		child.on('close', (status) => resolve({ status, stdout, stderr }));
	});
}

/** Google's token endpoint and the store API, on one port; the upload is refused. */
async function fakeStore(uploadStatus, uploadBody) {
	const server = createServer((req, res) => {
		req.resume();
		req.on('end', () => {
			if (req.url === '/token') {
				res.writeHead(200, { 'content-type': 'application/json' });
				res.end(JSON.stringify({ access_token: 'ACCESS_TOKEN_MUST_NOT_LEAK' }));
			} else {
				res.writeHead(uploadStatus, { 'content-type': 'application/json' });
				res.end(uploadBody);
			}
		});
	});
	await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
	return { server, base: `http://127.0.0.1:${server.address().port}` };
}

async function publishAgainst(uploadStatus, uploadBody) {
	const { server, base } = await fakeStore(uploadStatus, uploadBody);
	const { privateKey } = generateKeyPairSync('rsa', { modulusLength: 2048 });
	const zip = join(tmpdir(), `publish-extension-test-${process.pid}.zip`);
	await writeFile(zip, 'not really a zip');
	try {
		return await runAsync([zip], {
			CWS_API: base,
			CWS_PUBLISHER_ID: 'publisher',
			CWS_EXTENSION_ID: 'item',
			CWS_SERVICE_ACCOUNT: JSON.stringify({
				client_email: 'ci@example.iam.gserviceaccount.com',
				private_key: privateKey.export({ type: 'pkcs8', format: 'pem' }),
				token_uri: `${base}/token`
			})
		});
	} finally {
		server.close();
	}
}

test('a refused upload reports the store\'s reason, not a generic failure', async () => {
	const result = await publishAgainst(
		400,
		JSON.stringify({
			error: {
				code: 400,
				status: 'FAILED_PRECONDITION',
				message: 'Item is pending review.'
			}
		})
	);

	assert.equal(result.status, 1);
	assert.equal(
		result.stderr,
		'Chrome Web Store POST /upload/v2/publishers/publisher/items/item:upload?uploadType=media ' +
			'failed: HTTP 400 FAILED_PRECONDITION: Item is pending review.\n'
	);
	assert.equal(`${result.stdout}${result.stderr}`.includes('ACCESS_TOKEN_MUST_NOT_LEAK'), false);
});

test('a refused upload without an error envelope never echoes the body', async () => {
	const result = await publishAgainst(502, 'BODY_MUST_NOT_LEAK');

	assert.equal(result.status, 1);
	assert.match(result.stderr, /:upload\?uploadType=media failed: HTTP 502 \(no error details in the response\)\n$/);
	assert.equal(result.stderr.includes('BODY_MUST_NOT_LEAK'), false);
});
