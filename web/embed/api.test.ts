import assert from 'node:assert/strict';
import { test } from 'node:test';
import { EmbedApi, EmbedError, TokenStore, type StorageLike } from './api.ts';

class MemoryStorage implements StorageLike {
	readonly items = new Map<string, string>();
	getItem = (k: string) => this.items.get(k) ?? null;
	setItem = (k: string, v: string) => void this.items.set(k, v);
	removeItem = (k: string) => void this.items.delete(k);
}

interface Seen {
	url: string;
	init: RequestInit;
}

function gateway(answers: Array<Response | Error>) {
	const seen: Seen[] = [];
	const fetcher = (async (url: string, init: RequestInit) => {
		seen.push({ url, init });
		const next = answers.shift();
		if (!next) throw new Error(`unexpected request to ${url}`);
		if (next instanceof Error) throw next;
		return next;
	}) as typeof fetch;
	return { seen, fetcher };
}

const json = (status: number, body: unknown) =>
	new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });
const expired = () =>
	json(401, { error: { code: 'visitor_session_expired', message: 'the visitor session timed out' } });

function api(storage: StorageLike | null, answers: Array<Response | Error>) {
	const g = gateway(answers);
	const tokens = new TokenStore(() => storage, 'slot');
	return {
		...g,
		tokens,
		api: new EmbedApi({ base: 'https://gw.test', key: 'gwe_abc', lang: 'de', tokens, fetch: g.fetcher })
	};
}

function header(seen: Seen, name: string): string | undefined {
	return (seen.init.headers as Record<string, string>)[name];
}

test('starting a session stores the token and never sends cookies', async () => {
	const storage = new MemoryStorage();
	const t = api(storage, [json(201, { token: 'gwv_1', agent: { display: 'Ada' } })]);
	const started = await t.api.start();
	assert.equal(started.agent.display, 'Ada');
	assert.equal(storage.getItem('slot'), 'gwv_1');
	assert.equal(t.seen[0].url, 'https://gw.test/api/v0/embed/sessions');
	assert.equal(t.seen[0].init.credentials, 'omit');
	assert.equal(JSON.parse(t.seen[0].init.body as string).key, 'gwe_abc');
});

test('a message goes out with the bearer token and the chosen language', async () => {
	const storage = new MemoryStorage();
	storage.setItem('slot', 'gwv_1');
	const t = api(storage, [json(202, { turn_id: 'a', user_turn_id: 'u' })]);
	assert.deepEqual(await t.api.send('hello'), { restarted: false, agent: null });
	assert.equal(header(t.seen[0], 'authorization'), 'Bearer gwv_1');
	assert.equal(header(t.seen[0], 'accept-language'), 'de');
});

test('a first message starts the session by itself and names the agent', async () => {
	const t = api(new MemoryStorage(), [
		json(201, { token: 'gwv_new', agent: { display: 'Ada' } }),
		json(202, {})
	]);
	assert.deepEqual(await t.api.send('hello'), { restarted: false, agent: { display: 'Ada' } });
	assert.equal(header(t.seen[1], 'authorization'), 'Bearer gwv_new');
});

test('an expired session on send starts fresh and sends once more', async () => {
	const storage = new MemoryStorage();
	storage.setItem('slot', 'gwv_old');
	const t = api(storage, [
		expired(),
		json(201, { token: 'gwv_fresh', agent: { display: 'Ada' } }),
		json(202, {})
	]);
	assert.deepEqual(await t.api.send('hello'), { restarted: true, agent: { display: 'Ada' } });
	assert.equal(storage.getItem('slot'), 'gwv_fresh');
	assert.equal(header(t.seen[2], 'authorization'), 'Bearer gwv_fresh');
});

test('a session that is expired twice in a row surfaces the error', async () => {
	const storage = new MemoryStorage();
	storage.setItem('slot', 'gwv_old');
	const t = api(storage, [expired(), json(201, { token: 'gwv_2', agent: { display: 'A' } }), expired()]);
	await assert.rejects(t.api.send('hello'), (e: EmbedError) => e.code === 'visitor_session_expired');
});

test('resume returns null and forgets the token once the session timed out', async () => {
	const storage = new MemoryStorage();
	storage.setItem('slot', 'gwv_old');
	const t = api(storage, [expired()]);
	assert.equal(await t.api.resume(), null);
	assert.equal(storage.getItem('slot'), null);
});

test('resume without a token does not touch the network', async () => {
	const t = api(new MemoryStorage(), []);
	assert.equal(await t.api.resume(), null);
	assert.equal(t.seen.length, 0);
});

test('other refusals keep the gateway code', async () => {
	const t = api(new MemoryStorage(), [
		json(403, { error: { code: 'origin_not_allowed', message: 'add https://x' } })
	]);
	await assert.rejects(t.api.start(), (e: EmbedError) => e.code === 'origin_not_allowed' && e.status === 403);
});

test('a failed connection is a network error, not a crash', async () => {
	const t = api(new MemoryStorage(), [new TypeError('Failed to fetch')]);
	await assert.rejects(t.api.start(), (e: EmbedError) => e.code === 'network');
});

test('events yields the parsed frames of the stream', async () => {
	const storage = new MemoryStorage();
	storage.setItem('slot', 'gwv_1');
	const body = 'event: snapshot\ndata: {"turns":[]}\n\n: working\n\nevent: idle\ndata: {}\n\n';
	const t = api(storage, [new Response(body)]);
	const names: string[] = [];
	for await (const frame of t.api.events(new AbortController().signal)) names.push(frame.event);
	assert.deepEqual(names, ['snapshot', 'idle']);
	assert.equal(header(t.seen[0], 'authorization'), 'Bearer gwv_1');
});

test('the token store survives storage that throws, and a missing one', () => {
	const throwing: StorageLike = {
		getItem() {
			throw new Error('blocked');
		},
		setItem() {
			throw new Error('blocked');
		},
		removeItem() {
			throw new Error('blocked');
		}
	};
	for (const storage of [throwing, null]) {
		const store = new TokenStore(() => storage, 'slot');
		store.save('gwv_x');
		assert.equal(store.load(), 'gwv_x');
		store.clear();
		assert.equal(store.load(), null);
	}
});

test('the storage accessor itself may throw', () => {
	const store = new TokenStore(() => {
		throw new Error('SecurityError');
	}, 'slot');
	store.save('gwv_x');
	assert.equal(store.load(), 'gwv_x');
});

test('the secure field answers on the resume route, the code in the body only', async () => {
	const storage = new MemoryStorage();
	storage.setItem('slot', 'gwv_1');
	const t = api(storage, [json(202, { turn_id: 'a1' }), json(202, { turn_id: 'a1' })]);
	await t.api.answer('req-1', 'value', '481516');
	assert.equal(t.seen[0].url, 'https://gw.test/api/v0/embed/resume');
	assert.equal(header(t.seen[0], 'authorization'), 'Bearer gwv_1');
	assert.deepEqual(JSON.parse(t.seen[0].init.body as string), { request_id: 'req-1', decision: 'value', value: '481516' });
	await t.api.answer('req-1', 'deny');
	assert.deepEqual(JSON.parse(t.seen[1].init.body as string), { request_id: 'req-1', decision: 'deny' });
});

test('an identity token goes out once, right after the session starts', async () => {
	const t = api(new MemoryStorage(), [
		json(201, { token: 'gwv_new', agent: { display: 'Ada' } }),
		json(200, { slots: ['verified'] }),
		json(202, {}),
		json(202, {})
	]);
	await t.api.setIdentity('eyJ.signed.token');
	assert.equal(t.seen.length, 0, 'no conversation yet, nothing to vouch for');
	await t.api.send('hello');
	await t.api.send('again');
	const urls = t.seen.map((s) => s.url.replace('https://gw.test', ''));
	assert.deepEqual(urls, ['/api/v0/embed/sessions', '/api/v0/embed/identity', '/api/v0/embed/messages', '/api/v0/embed/messages']);
	assert.equal(header(t.seen[1], 'authorization'), 'Bearer gwv_new');
	assert.deepEqual(JSON.parse(t.seen[1].init.body as string), { token: 'eyJ.signed.token' });
});

test('an identity set during a conversation is sent at once, and a refusal does not break it', async () => {
	const storage = new MemoryStorage();
	storage.setItem('slot', 'gwv_1');
	const t = api(storage, [json(401, { error: { code: 'identity_token_invalid', message: 'it has expired' } })]);
	const warned: string[] = [];
	const original = console.warn;
	console.warn = (...args: unknown[]) => void warned.push(args.join(' '));
	try {
		await t.api.setIdentity('eyJ.expired');
	} finally {
		console.warn = original;
	}
	assert.equal(t.seen[0].url, 'https://gw.test/api/v0/embed/identity');
	assert.match(warned[0] ?? '', /identity_token_invalid/);
});

test('the agent is described from its key before any conversation', async () => {
	const t = api(new MemoryStorage(), [
		json(200, { agent: { display: 'Ada', color: '#0b6bcb', voice: { input: true, output: false } } })
	]);
	const agent = await t.api.describe();
	assert.deepEqual(agent.voice, { input: true, output: false });
	assert.equal(t.seen[0].url, 'https://gw.test/api/v0/embed/agent');
	assert.equal(header(t.seen[0], 'authorization'), undefined);
});

test('a recording starts a conversation, goes up as WAV and comes back as text only', async () => {
	const storage = new MemoryStorage();
	const t = api(storage, [json(201, { token: 'gwv_1', agent: { display: 'Ada' } }), json(200, { text: 'Hallo' })]);
	const wav = new ArrayBuffer(48);
	const result = await t.api.transcribe(wav);
	assert.equal(result.text, 'Hallo');
	assert.equal(result.agent?.display, 'Ada');
	assert.equal(t.seen[1].url, 'https://gw.test/api/v0/embed/transcribe');
	assert.equal(header(t.seen[1], 'content-type'), 'audio/wav');
	assert.equal(header(t.seen[1], 'authorization'), 'Bearer gwv_1');
	assert.equal(t.seen[1].init.body, wav);
	assert.equal(t.seen.length, 2, 'nothing is sent as a message');
});

test('an answer is spoken by its turn id, and an unspeakable one is nothing', async () => {
	const storage = new MemoryStorage();
	storage.setItem('slot', 'gwv_1');
	const t = api(storage, [new Response(new Uint8Array([1, 2, 3]), { status: 200 }), new Response(null, { status: 204 })]);
	const audio = await t.api.speak('t1');
	assert.equal(audio?.byteLength, 3);
	assert.deepEqual(JSON.parse(t.seen[0].init.body as string), { turn_id: 't1' });
	assert.equal(await t.api.speak('t2'), null);
	assert.equal(t.api.recorderUrl, 'https://gw.test/api/v0/embed/recorder.js');
});
