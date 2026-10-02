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
	assert.deepEqual(await t.api.send('hello'), { restarted: false });
	assert.equal(header(t.seen[0], 'authorization'), 'Bearer gwv_1');
	assert.equal(header(t.seen[0], 'accept-language'), 'de');
});

test('a first message starts the session by itself', async () => {
	const t = api(new MemoryStorage(), [
		json(201, { token: 'gwv_new', agent: { display: 'Ada' } }),
		json(202, {})
	]);
	await t.api.send('hello');
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
	assert.deepEqual(await t.api.send('hello'), { restarted: true });
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
