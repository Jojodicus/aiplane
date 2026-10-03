import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';

import { cleanSpec, ensureShape, type AgentResources, type Grant, type Spec } from './agents.ts';
import {
	SLOT_KINDS,
	TEMPLATES,
	TONE_LINES,
	abilities,
	applyTemplate,
	asStep,
	checklist,
	deriveBind,
	identFrom,
	identityWriter,
	isBlank,
	liveUses,
	newSecret,
	originOf,
	readBasics,
	readHandoffs,
	readIdentity,
	readScope,
	readColor,
	readSite,
	readVoice,
	readSlots,
	sectionStatus,
	setAbility,
	setKnowledge,
	slotDef,
	slotKind,
	slotsForIdentity,
	stepForPath,
	voiceMissing,
	summary,
	suggestedMethod,
	suggestedRules,
	suggestedSlotRows,
	templateSpec,
	tierOf,
	topicsNeedingIdentity,
	unique,
	writeBasics,
	writeHandoffs,
	writeIdentity,
	writeScope,
	writeColor,
	writeSite,
	writeVoice,
	writeSlots,
	type Basics,
	type Handoffs,
	type Identity,
	type SlotRow
} from './agent-setup.ts';

const tr = (key: string) => `«${key}»`;
const labels = { email: 'E-mail', name: 'Name', customerNumber: 'Customer number' };

/** What the advanced editor makes of a spec: shaped for its forms, then cleaned for saving. */
const sortKeys = (v: unknown): unknown =>
	Array.isArray(v) ? v.map(sortKeys) : v && typeof v === 'object' ? Object.fromEntries(Object.keys(v).sort().map((k) => [k, sortKeys((v as Spec)[k])])) : v;
/** What the advanced editor and a save make of a spec: cleaned for saving, stored by the server (which sorts the keys), shaped again for the forms. */
const throughEditor = (spec: Spec): Spec => ensureShape(sortKeys(cleanSpec(ensureShape(spec))) as Spec);

test('names become spec identifiers, unique within what is taken', () => {
	assert.equal(identFrom('E-Mail-Adresse', 'detail'), 'e_mail_adresse');
	assert.equal(identFrom('Größe', 'detail'), 'grosse');
	assert.equal(identFrom('2nd line', 'detail'), 'detail_2nd_line');
	assert.equal(identFrom('账单', 'handoff'), 'handoff');
	assert.equal(identFrom('x'.repeat(80), 'd').length, 48);
	assert.equal(unique('topic', ['topic', 'topic_2']), 'topic_3');
	assert.equal(unique('fresh', ['topic']), 'fresh');
	assert.equal(asStep('scope'), 'scope');
	assert.equal(asStep('nope'), null);
});

test('task and tone round-trip through the response instructions', () => {
	const model: Basics = {
		name: 'Harald',
		task: 'Help croit customers.',
		tones: ['friendly', 'formal'],
		language: 'de',
		extra: 'Never use emojis.',
		pool: 'chat-large'
	};
	const spec = ensureShape({});
	writeBasics(spec, model);
	assert.equal(spec.main.instructions.response, `${TONE_LINES.friendly}\n${TONE_LINES.formal}\nAlways answer in German.\nNever use emojis.`);
	assert.equal(spec.profile.display, 'Harald');
	assert.equal(spec.main.pool, 'chat-large');
	assert.deepEqual(readBasics(throughEditor(spec)), model);
});

test('free-written response instructions stay as they are', () => {
	const spec = { main: { instructions: { response: 'Friendly, short, in the visitor’s language.' } } };
	const b = readBasics(spec);
	assert.deepEqual(b.tones, []);
	assert.equal(b.language, null);
	assert.equal(b.extra, 'Friendly, short, in the visitor’s language.');
	writeBasics(spec, b);
	assert.equal((spec as Spec).main.instructions.response, 'Friendly, short, in the visitor’s language.');
});

test('a model choice is the admin-mapped pool it names', () => {
	const tiers = { fast: 'small', balanced: 'mid', thorough: null };
	assert.equal(tierOf('mid', tiers), 'balanced');
	assert.equal(tierOf('other', tiers), null);
	assert.equal(tierOf('', tiers), null);
});

test('the scope step writes #115 scope and drops an empty one', () => {
	const spec: Spec = { scope: { classifier_pool: 'guard' } };
	writeScope(spec, { topics: [' Ceph ', '', 'Licences'], refusal: 'Only croit.', strict: true });
	assert.deepEqual(spec.scope, { topics: ['Ceph', 'Licences'], refusal: 'Only croit.', strict: true, classifier_pool: 'guard' });
	assert.deepEqual(readScope(throughEditor(spec)), { topics: ['Ceph', 'Licences'], refusal: 'Only croit.', strict: true });

	const empty: Spec = { scope: { topics: [] } };
	writeScope(empty, { topics: [], refusal: '', strict: false });
	assert.equal(empty.scope, undefined);
});

test('every friendly slot kind reads back as itself, through the advanced editor too', () => {
	for (const kind of SLOT_KINDS) {
		const def = slotDef(kind, 'Label', kind === 'choice' ? ['a', 'b'] : []);
		assert.equal(slotKind(def), kind, kind);
		assert.equal(slotKind(throughEditor({ state: { x: def } }).state.x), kind, `${kind} after the editor`);
	}
	assert.equal(slotKind({ type: 'string', max_length: 50, set_by: ['llm'] }), 'custom');
	assert.equal(slotKind({ type: 'email', set_by: ['host'] }), 'custom');
});

test('details round-trip; a fresh row takes its key from its label, managed and custom slots survive', () => {
	const custom = { type: 'string', min_length: 3, set_by: ['llm'] };
	const spec: Spec = {
		state: {
			email: slotDef('email', 'E-mail'),
			legacy: custom,
			verified: { type: 'subject', set_by: ['host'] },
			topic: { type: 'enum', values: ['x'], set_by: ['llm'] }
		}
	};
	const rows = readSlots(spec);
	assert.deepEqual(rows.map((r) => [r.key, r.kind]), [['email', 'email'], ['legacy', 'custom']]);
	const next: SlotRow[] = [...rows, { key: '', label: 'Customer number', kind: 'customer_number', values: [], fresh: true }, { key: '', label: 'Plan', kind: 'choice', values: ['basic', 'pro'], fresh: true }];
	writeSlots(spec, next);
	assert.deepEqual(Object.keys(spec.state), ['email', 'legacy', 'customer_number', 'plan', 'verified', 'topic']);
	const back = readSlots(throughEditor(spec));
	assert.deepEqual(back, [
		{ key: 'email', label: 'E-mail', kind: 'email', values: [] },
		{ key: 'legacy', label: 'Legacy', kind: 'custom', values: [] },
		{ key: 'customer_number', label: 'Customer number', kind: 'customer_number', values: [] },
		{ key: 'plan', label: 'Plan', kind: 'choice', values: ['basic', 'pro'] }
	], 'the order the person gave survives the server sorting the keys');
	assert.deepEqual(spec.state.legacy, { ...custom, order: 1 });
	writeSlots(spec, back.filter((r) => r.key !== 'legacy'));
	assert.equal(spec.state.legacy, undefined);
});

const identity = (over: Partial<Identity>): Identity => ({ method: 'none', connector: '', tool: '', issuer: '', audience: '', secret: '', secretSet: false, others: 0, ...over });

test('each identity method writes its verifier and the confirmed slot, and reads back', () => {
	const spec: Spec = {};
	writeIdentity(spec, identity({ method: 'email_code', connector: 'erp' }), labels);
	assert.deepEqual(spec.verifiers.identity, { kind: 'mcp_code', email_slot: 'email', writes: { verified: 'result' }, connector: 'erp' });
	assert.deepEqual(spec.state.verified, { type: 'subject', set_by: ['verifier:identity'] });
	assert.equal(spec.state.email.type, 'email');
	assert.deepEqual(slotsForIdentity(spec), ['email']);
	assert.equal(readIdentity(throughEditor(spec)).connector, 'erp');
	assert.equal(readIdentity(spec).method, 'email_code');

	writeIdentity(spec, identity({ method: 'customer_lookup', tool: 'mcp__erp__find' }), labels);
	assert.equal(spec.verifiers.identity.kind, 'lookup');
	assert.equal(spec.verifiers.identity.assurance, 'low');
	assert.deepEqual(slotsForIdentity(spec).sort(), ['customer_number', 'name']);
	assert.equal(slotKind(spec.state.customer_number), 'customer_number');
	assert.deepEqual(readIdentity(throughEditor(spec)).tool, 'mcp__erp__find');

	writeIdentity(spec, identity({ method: 'signed_in', issuer: 'https://shop.example', audience: 'support', secret: 'x'.repeat(40) }), labels);
	assert.equal(spec.verifiers.identity.algorithm, 'HS256');
	assert.deepEqual(spec.state.verified.set_by, ['host']);
	assert.equal(identityWriter(spec), 'host');
	const read = readIdentity(throughEditor(spec));
	assert.equal(read.issuer, 'https://shop.example');
	assert.equal(read.audience, 'support');

	writeIdentity(spec, identity({ method: 'none' }), labels);
	assert.equal(spec.verifiers, undefined);
	assert.equal(spec.state.verified, undefined);
	assert.equal(readIdentity(spec).method, 'none');
});

test('a stored secret stays sealed until a new one is typed', () => {
	const spec: Spec = { verifiers: { identity: { kind: 'host_jwt', algorithm: 'HS256', secret_sealed: 'sealed', issuer: 'i', audience: 'a' } } };
	const read = readIdentity(spec);
	assert.equal(read.secretSet, true);
	writeIdentity(spec, read, labels);
	assert.equal(spec.verifiers.identity.secret_sealed, 'sealed');
	assert.equal(spec.verifiers.identity.secret, undefined);
	writeIdentity(spec, { ...read, secret: 'n'.repeat(40) }, labels);
	assert.equal(spec.verifiers.identity.secret_sealed, undefined);
	assert.equal(newSecret().length, 48);
});

test('changing the identity method moves the hand-off gates to the new writer', () => {
	const spec: Spec = {};
	writeIdentity(spec, identity({ method: 'email_code' }), labels);
	writeHandoffs(spec, { rules: [{ route: null, topic: 'Invoices', identity: true, target: { kind: 'agent', id: 'a1' }, bind: {} }], fallback: false, custom: [] });
	assert.deepEqual(spec.routes.invoices.when.all[2], { slot: 'verified', provenance: 'verifier:identity' });
	writeIdentity(spec, identity({ method: 'signed_in' }), labels);
	assert.deepEqual(spec.routes.invoices.when.all[2], { slot: 'verified', provenance: 'host' });
	assert.deepEqual(topicsNeedingIdentity(spec), ['Invoices']);
});

test('hand-off sentences become routes with derived gates, and read back unchanged', () => {
	const spec: Spec = { routes: { legacy: { when: { slot: 'x', set: true }, agent: 'z', task: 't' } }, state: { x: { type: 'string', set_by: ['llm'] } } };
	writeIdentity(spec, identity({ method: 'email_code', connector: 'erp' }), labels);
	const model: Handoffs = {
		rules: [
			{ route: null, topic: 'Invoices', identity: true, target: { kind: 'agent', id: 'billing-id' }, bind: { customer: 'state.verified.customer' } },
			{ route: null, topic: 'Technik', identity: false, target: { kind: 'human' }, bind: {} },
			{ route: null, topic: '  ', identity: false, target: { kind: 'human' }, bind: {} }
		],
		fallback: true,
		custom: ['legacy']
	};
	writeHandoffs(spec, model);
	assert.deepEqual(Object.keys(spec.routes), ['invoices', 'technik', 'legacy', 'fallback']);
	assert.deepEqual(spec.router, { kind: 'rules', order: ['invoices', 'technik', 'legacy', 'fallback'] });
	assert.deepEqual(spec.routes.invoices, {
		description: 'Invoices',
		when: { all: [{ slot: 'topic', eq: 'Invoices' }, { slot: 'request', set: true }, { slot: 'verified', provenance: 'verifier:identity' }] },
		agent: 'billing-id',
		task: 'Request about {topic}: {request}',
		bind: { customer: 'state.verified.customer' }
	});
	assert.deepEqual(spec.routes.technik.human, {});
	assert.deepEqual(spec.state.topic.values, ['Invoices', 'Technik']);
	assert.equal(spec.state.request.type, 'string');

	const back = readHandoffs(throughEditor(spec));
	assert.deepEqual(back, {
		rules: [
			{ route: 'invoices', topic: 'Invoices', identity: true, target: { kind: 'agent', id: 'billing-id' }, bind: { customer: 'state.verified.customer' } },
			{ route: 'technik', topic: 'Technik', identity: false, target: { kind: 'human' }, bind: {} }
		],
		fallback: true,
		custom: ['legacy']
	});

	writeHandoffs(spec, { rules: [], fallback: false, custom: ['legacy'] });
	assert.deepEqual(Object.keys(spec.routes), ['legacy']);
	assert.equal(spec.state.topic, undefined);
	assert.equal(spec.state.request, undefined);
	assert.deepEqual(spec.router, { kind: 'rules', order: ['legacy'] });
	writeHandoffs(spec, { rules: [], fallback: false, custom: [] });
	assert.equal(spec.router, undefined);
});

test('a specialist’s route values come from trusted slots or the confirmed identity', () => {
	const specialist = { main: { tool_resources: { mcp__erp__invoices: { bind: { customer_id: 'route.customer', region: { const: 'eu' } } }, other: { bind: { x: 'route.plan' } } } } };
	const spec: Spec = { state: { plan: { type: 'string', set_by: ['host'] } } };
	assert.deepEqual(deriveBind(specialist, spec), { bind: { plan: 'state.plan' }, missing: ['customer'] });
	spec.state.verified = { type: 'subject', set_by: ['host'] };
	assert.deepEqual(deriveBind(specialist, spec), { bind: { customer: 'state.verified.customer', plan: 'state.plan' }, missing: [] });
	assert.deepEqual(deriveBind(null, spec), { bind: {}, missing: [] });
});

const resources: AgentResources = {
	pools: ['mid'],
	tools: [
		{ id: 'search_web', name: 'Web search', description: 'Searches the web' },
		{ id: 'rag_search', name: 'Knowledge search', description: null },
		{ id: 'rag_list_collections', name: 'List collections', description: null }
	],
	connectors: [{ key: 'jira', name: 'Jira', tools: ['mcp__jira__create', 'mcp__jira__get'] }],
	skills: ['brand-voice'],
	rag_collections: [{ id: 7, name: 'croit docs' }]
};
const grant = (kind: Grant['kind'], ref: string): Grant => ({ kind, ref, granted_by: 'admin', granted_at: '' });

test('ability cards: what the manager may grant, and what the agent has that they may not', () => {
	const spec: Spec = { main: { tools: ['search_web', 'mcp__erp__invoices', 'netcheck'], skills: ['secret-skill'] } };
	const cards = abilities(spec, [grant('rag_collection', '7'), grant('rag_collection', '9')], resources);
	const view = cards.map((c) => `${c.kind}:${c.ref}:${c.on ? 'on' : 'off'}:${c.holdable ? 'mine' : 'locked'}`);
	assert.deepEqual(view, [
		'rag_collection:7:on:mine',
		'rag_collection:9:on:locked',
		'tool:search_web:on:mine',
		'tool:netcheck:on:locked',
		'connector:jira:off:mine',
		'connector:erp:on:locked',
		'skill:brand-voice:off:mine',
		'skill:secret-skill:on:locked'
	]);
});

test('switching abilities edits the spec, and knowledge binds a single collection', () => {
	const spec: Spec = ensureShape({});
	setAbility(spec, { kind: 'connector', ref: 'jira', tools: ['mcp__jira__create', 'mcp__jira__get'] }, true);
	setAbility(spec, { kind: 'skill', ref: 'brand-voice', tools: [] }, true);
	setAbility(spec, { kind: 'tool', ref: 'search_web', tools: ['search_web'] }, true);
	setKnowledge(spec, ['croit docs'], true);
	assert.deepEqual(spec.main.tools, ['mcp__jira__create', 'mcp__jira__get', 'search_web', 'rag_search']);
	assert.deepEqual(spec.main.tool_resources.rag_search, { bind: { collection: { const: 'croit docs' } } });
	const after = abilities(throughEditor(spec), [grant('rag_collection', '7')], resources).filter((c) => c.on).map((c) => c.ref);
	assert.deepEqual(after, ['7', 'search_web', 'jira', 'brand-voice']);

	setKnowledge(spec, ['croit docs', 'faq'], true);
	assert.deepEqual(spec.main.tool_resources.rag_search.bind, {});
	assert.ok(spec.main.tools.includes('rag_list_collections'));
	setKnowledge(spec, [], true);
	setAbility(spec, { kind: 'connector', ref: 'jira', tools: [] }, false);
	assert.deepEqual(spec.main.tools, ['search_web']);
	assert.equal(spec.main.tool_resources.rag_search, undefined);
});

test('a grant the published version relies on is kept', () => {
	const live = { main: { pool: 'mid', tools: ['rag_search', 'mcp__jira__get'], skills: ['brand-voice'] }, verifiers: { identity: { kind: 'mcp_code', connector: 'erp' } } };
	assert.ok(liveUses(live, 'pool', 'mid'));
	assert.ok(liveUses(live, 'connector', 'jira'));
	assert.ok(liveUses(live, 'connector', 'erp'));
	assert.ok(liveUses(live, 'rag_collection', '7'));
	assert.ok(liveUses(live, 'skill', 'brand-voice'));
	assert.ok(!liveUses(live, 'tool', 'search_web'));
	assert.ok(!liveUses(null, 'tool', 'search_web'));
	assert.ok(liveUses({ publish: { voice: { speech_pool: 'tts' } } }, 'pool', 'tts'), 'a live voice pool stays granted');
});

test('the website step keeps only web origins', () => {
	assert.equal(originOf('www.croit.io/de/kontakt'), 'https://www.croit.io');
	assert.equal(originOf('http://localhost:3000/x'), 'http://localhost:3000');
	assert.equal(originOf('ftp://x'), null);
	assert.equal(originOf('not a url'), null);
	const spec: Spec = {};
	writeSite(spec, ['https://www.croit.io/x', 'www.croit.io', 'nonsense here']);
	assert.deepEqual(readSite(throughEditor(spec)), ['https://www.croit.io']);
	writeSite(spec, []);
	assert.equal(spec.publish, undefined);
});

test('the website step binds the widget colour and voice and writes back what it reads', () => {
	const spec: Spec = { profile: { display: 'Ada' } };
	writeColor(spec, '#0B6BCB');
	assert.equal(readColor(throughEditor(spec)), '#0b6bcb');
	writeColor(spec, 'blue');
	assert.deepEqual(spec.profile, { display: 'Ada' }, 'a non-colour clears it');

	const voice = { input: true, output: true, voice: 'nova', transcriptionPool: 'stt', speechPool: 'tts' };
	writeVoice(spec, voice);
	assert.deepEqual(spec.publish.voice, { input: true, output: true, voice: 'nova', transcription_pool: 'stt', speech_pool: 'tts' });
	assert.deepEqual(readVoice(throughEditor(spec)), voice);
	assert.deepEqual(voiceMissing({ ...voice, speechPool: '' }), ['speech']);
	assert.deepEqual(voiceMissing({ ...voice, input: false, transcriptionPool: '' }), []);

	writeVoice(spec, { input: false, output: false, voice: '', transcriptionPool: '', speechPool: '' });
	assert.equal(spec.publish, undefined, 'nothing chosen leaves no publish block');
	assert.equal(stepForPath('publish.voice.speech_pool'), 'site');
	assert.equal(stepForPath('profile.color'), 'site');
	assert.equal(stepForPath('profile.display'), 'basics');
});

test('every template is a starter spec the steps read without leftovers', () => {
	for (const key of TEMPLATES) {
		const spec = templateSpec(key, tr);
		assert.ok(!JSON.stringify(spec).includes('"@'), `${key} kept a catalog key`);
		const b = readBasics(spec);
		assert.equal(b.extra, '', `${key}: every response line is a chip`);
		const h = readHandoffs(spec);
		assert.deepEqual(h.custom, [], `${key}: every route is a rule or the fallback`);
		assert.ok(readSlots(spec).every((r) => r.kind !== 'custom'), `${key}: every detail has a friendly kind`);
		const again = JSON.parse(JSON.stringify(spec));
		writeBasics(again, b);
		writeScope(again, readScope(spec));
		writeSlots(again, readSlots(spec));
		writeIdentity(again, readIdentity(spec), labels);
		writeHandoffs(again, h);
		writeSite(again, readSite(spec));
		writeColor(again, readColor(spec));
		writeVoice(again, readVoice(spec));
		assert.deepEqual(throughEditor(again), throughEditor(spec), `${key}: the steps write back what they read`);
	}
	assert.equal(templateSpec('support', tr).main.instructions.orchestration, '«agents-tpl-support-task»');
	assert.deepEqual(templateSpec('blank', tr), {});
});

test('a template keeps the name and the model already chosen', () => {
	const spec = applyTemplate({ profile: { display: 'Harald' }, main: { pool: 'mid', tools: ['x'] } }, 'faq', tr);
	assert.equal(spec.profile.display, 'Harald');
	assert.equal(spec.main.pool, 'mid');
	assert.equal(spec.main.tools, undefined);
	assert.ok(isBlank(ensureShape({ profile: { display: 'Harald' }, main: { pool: 'mid' } })));
	assert.ok(!isBlank(spec));
});

test('the checklist points each open item at the step that fixes it', () => {
	const spec = templateSpec('support', tr);
	const todos = checklist(spec, [{ path: 'routes.fallback.human', message: 'bad' }, { path: 'finish', message: 'odd' }]);
	assert.deepEqual(
		todos.map((t) => [t.step, t.key ?? t.message, t.blocking]),
		[
			['basics', 'agents-setup-todo-model', true],
			['identity', 'agents-setup-todo-identity-connector', true],
			['site', 'agents-setup-todo-site', false],
			['routes', 'routes.fallback.human: bad', true],
			[null, 'finish: odd', true]
		]
	);
	assert.equal(sectionStatus('identity', spec, todos), 'open');
	assert.equal(sectionStatus('slots', spec, todos), 'done');
	assert.equal(sectionStatus('abilities', spec, todos), 'optional');

	writeScope(spec, { topics: [], refusal: '', strict: true });
	assert.ok(checklist(spec).some((t) => t.step === 'scope' && t.key === 'agents-setup-strict-needs'));
	assert.equal(stepForPath('state.topic.values'), 'routes');
	assert.equal(stepForPath('state.email'), 'slots');
	assert.equal(stepForPath('main.tools[1]'), 'abilities');
});

test('each section sums itself up in one line', () => {
	const spec = templateSpec('support', tr);
	writeScope(spec, { topics: ['Ceph', 'Licences'], refusal: 'No.', strict: true });
	writeHandoffs(spec, { rules: [{ route: null, topic: 'Invoices', identity: false, target: { kind: 'agent', id: 'b1' }, bind: {} }], fallback: true, custom: [] });
	const ctx = {
		tr: (key: string, args?: Record<string, string | number>) => (args ? `${key}(${Object.values(args).join('|')})` : key),
		grants: [grant('rag_collection', '7')],
		resources: { ...resources, tiers: { fast: null, balanced: 'mid', thorough: null } },
		agents: [{ id: 'b1', name: 'billing', display: 'Billing' }]
	};
	spec.main.pool = 'mid';
	assert.equal(summary('basics', spec, ctx), 'agents-setup-sum-basics(«agents-tpl-support-task»|agents-setup-model-balanced)');
	assert.equal(summary('scope', spec, ctx), 'agents-setup-sum-scope-strict(Ceph, Licences)');
	assert.equal(summary('abilities', spec, ctx), 'croit docs');
	assert.equal(summary('slots', spec, ctx), '«agents-tpl-slot-name», «agents-tpl-slot-email»');
	assert.equal(summary('identity', spec, ctx), 'agents-setup-identity-email_code');
	assert.equal(summary('routes', spec, ctx), 'Invoices → Billing · agents-setup-sum-routes-other(agents-setup-rule-person)');
	assert.equal(summary('site', spec, ctx), 'agents-setup-sum-site-none');
});

test('a proposal maps onto the steps: identity cards, friendly details, hand-off sentences', () => {
	assert.equal(suggestedMethod('website_login'), 'signed_in');
	assert.equal(suggestedMethod('lookup'), 'customer_lookup');
	assert.equal(suggestedMethod('carrier_pigeon'), null);

	const existing: SlotRow[] = [{ key: 'email', label: 'E-mail', kind: 'email', values: [] }];
	const rows = suggestedSlotRows(
		[
			{ name: 'email', label: 'E-mail', def: { type: 'email', set_by: ['llm'] } },
			{ name: 'order_number', label: 'Order number', def: { type: 'string', max_length: 500, set_by: ['llm'] } },
			{ name: 'quantity', label: 'Quantity', def: { type: 'integer', set_by: ['llm'] } },
			{ name: 'plan', label: 'Plan', def: { type: 'enum', values: ['basic', 'pro'], set_by: ['llm'] } },
			{ name: 'topic', label: 'Topic', def: { type: 'string', set_by: ['llm'] } }
		],
		existing
	);
	assert.deepEqual(rows.map((r) => [r.key, r.kind, r.values]), [['order_number', 'text', []], ['quantity', 'whole_number', []], ['plan', 'choice', ['basic', 'pro']]]);
	const spec: Spec = {};
	writeSlots(spec, [...existing, ...rows]);
	assert.deepEqual(readSlots(throughEditor(spec)).map((r) => r.kind), ['email', 'text', 'whole_number', 'choice']);

	const rules = suggestedRules([{ topic: 'Invoices', target: 'b1' }, { topic: 'invoices', target: 'human' }, { topic: 'Returns', target: 'human' }], []);
	assert.deepEqual(rules.map((r) => [r.topic, r.target]), [['Invoices', { kind: 'agent', id: 'b1' }], ['Returns', { kind: 'human' }]]);
	assert.deepEqual(suggestedRules([{ topic: 'Returns', target: 'human' }], rules), []);
});

test('details keep the order they were given, and unordered slots follow by name', () => {
	const spec: Spec = {
		state: {
			zeta: { type: 'string', set_by: ['llm'] },
			alpha: { type: 'string', set_by: ['llm'] },
			second: { ...slotDef('text', 'Second'), order: 1 },
			first: { ...slotDef('email', 'First'), order: 0 }
		}
	};
	assert.deepEqual(readSlots(throughEditor(spec)).map((r) => r.key), ['first', 'second', 'alpha', 'zeta']);
	const rows = readSlots(spec);
	writeSlots(spec, [rows[1], rows[0], ...rows.slice(2)]);
	assert.deepEqual(readSlots(throughEditor(spec)).map((r) => r.key), ['second', 'first', 'alpha', 'zeta']);
	assert.equal(slotKind(spec.state.first), 'email', 'the order is not part of the friendly kind');
});

test('a draft the agent architect wrote reads as hand-off sentences and friendly details, not "advanced"', () => {
	// Written by `apply_changes` on the server; `review/tests.rs` pins it.
	const draft = JSON.parse(readFileSync(new URL('./fixtures/architect-draft.json', import.meta.url), 'utf8')) as Spec;
	const h = readHandoffs(throughEditor(draft));
	assert.deepEqual(h.custom, []);
	assert.equal(h.fallback, true);
	assert.deepEqual(
		h.rules.map((r) => [r.topic, r.target, r.identity]),
		[
			['Invoices', { kind: 'agent', id: 'billing-agent' }, false],
			['Complaints', { kind: 'human' }, false]
		]
	);
	assert.deepEqual(
		readSlots(throughEditor(draft)).map((r) => [r.key, r.kind]),
		[
			['order', 'text'],
			['issue', 'choice']
		]
	);
	const again = structuredClone(draft);
	writeHandoffs(again, h);
	assert.deepEqual(sortKeys(again.routes), sortKeys(draft.routes), 'writing the rules back changes nothing');
	assert.deepEqual(sortKeys(again.state), sortKeys(draft.state));
});
