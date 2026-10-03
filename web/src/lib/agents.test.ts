import assert from 'node:assert/strict';
import test from 'node:test';

import { ApiError } from './api.ts';
import {
	bindSource,
	cleanSpec,
	condKind,
	describeBindSource,
	ensureShape,
	issuesAt,
	issuesUnder,
	parseBindSource,
	parseSpecError,
	renameKey,
	renameRoute,
	removeRoute,
	freshName,
	writerOptions,
	slotValueFromText,
	splitList,
	slotInfos,
	suspensionLabel,
	testTurnLabel,
	type SpecIssue
} from './agents.ts';

const issues: SpecIssue[] = [
	{ path: 'main.pool', message: 'pool `x` is not granted' },
	{ path: 'main.tools[0]', message: 'tool `a` is not granted' },
	{ path: 'main.tools[1]', message: 'tool `b` is not granted' },
	{ path: 'routes.billing.when.all[1].slot', message: 'no slot `z`' },
	{ path: '', message: 'the spec is not an object' },
	{ path: 'main_other', message: 'unrelated' }
];

test('issues are found at a path exactly, or at it and below it', () => {
	assert.deepEqual(issuesAt(issues, 'main.pool'), ['pool `x` is not granted']);
	assert.deepEqual(issuesAt(issues, 'main'), []);
	assert.deepEqual(
		issuesUnder(issues, 'main.tools').map((i) => i.path),
		['main.tools[0]', 'main.tools[1]']
	);
	assert.deepEqual(
		issuesUnder(issues, 'routes.billing').map((i) => i.path),
		['routes.billing.when.all[1].slot']
	);
	assert.deepEqual(
		issuesUnder(issues, 'main').map((i) => i.path),
		['main.pool', 'main.tools[0]', 'main.tools[1]'],
		'`main_other` is not below `main`'
	);
});

test('a 422 invalid_agent_spec becomes a list of path-keyed issues', () => {
	const body = JSON.stringify({
		error: {
			message: 'cannot save the draft: at `main.pool`, bad',
			code: 'invalid_agent_spec',
			issues: [{ path: 'main.pool', message: 'bad' }]
		}
	});
	const parsed = parseSpecError(new ApiError(422, `422 Unprocessable Entity — ${body}`, 'invalid_agent_spec'));
	assert.equal(parsed.status, 422);
	assert.equal(parsed.code, 'invalid_agent_spec');
	assert.deepEqual(parsed.issues, [{ path: 'main.pool', message: 'bad' }]);
	assert.match(parsed.message, /cannot save the draft/);
});

test('other failures keep the server message and carry no issues', () => {
	const body = JSON.stringify({ error: { message: 'you can read this agent', code: 'agent_write_required' } });
	const parsed = parseSpecError(new ApiError(403, `403 Forbidden — ${body}`, 'agent_write_required'));
	assert.equal(parsed.code, 'agent_write_required');
	assert.equal(parsed.message, 'you can read this agent');
	assert.deepEqual(parsed.issues, []);

	const plain = parseSpecError(new Error('network down'));
	assert.equal(plain.message, 'network down');
	assert.equal(plain.status, 0);
});

test('editing shape is filled in without disturbing what is there', () => {
	const shaped = ensureShape({ main: { pool: 'p' }, extra: 1 });
	assert.deepEqual(shaped.main.tools, []);
	assert.deepEqual(shaped.main.skills, []);
	assert.deepEqual(shaped.main.instructions, { orchestration: '', response: '' });
	assert.deepEqual(shaped.state, {});
	assert.deepEqual(shaped.routes, {});
	assert.equal(shaped.main.pool, 'p');
	assert.equal(shaped.extra, 1);
	assert.deepEqual(ensureShape({}).main.budget, {});
});

test('cleaning drops blanks but keeps what the validator must see', () => {
	const cleaned = cleanSpec({
		profile: { display: '', color: '' },
		main: {
			pool: 'p',
			instructions: { orchestration: 'go', response: '' },
			tools: [],
			skills: [],
			tool_resources: { t: { bind: {}, permission: '' } },
			budget: { rounds: null, seconds: '', tokens: 5 }
		},
		state: { a: { type: '', set_by: [] }, b: { type: 'string', description: '' } },
		routes: { r: { when: { all: [] }, agent: 'x', task: '', bind: {} } },
		finish: { schema: { type: 'object', required: [] } },
		publish: { origins: [], idle_ttl: '' },
		on_tool_unavailable: ''
	});
	assert.deepEqual(cleaned, {
		main: {
			pool: 'p',
			instructions: { orchestration: 'go' },
			tool_resources: { t: {} },
			budget: { tokens: 5 }
		},
		state: { a: {}, b: { type: 'string' } },
		routes: { r: { when: { all: [] }, agent: 'x' } },
		finish: { schema: { type: 'object', required: [] } }
	});
	assert.deepEqual(cleanSpec({}), { main: {} }, 'a spec always names its main agent');
});

test('cleaning does not mutate its input', () => {
	const input = { main: { pool: '', tools: [] } };
	cleanSpec(input);
	assert.deepEqual(input, { main: { pool: '', tools: [] } });
});

test('a gate node is a combinator or a leaf', () => {
	assert.equal(condKind({ all: [] }), 'all');
	assert.equal(condKind({ any: [] }), 'any');
	assert.equal(condKind({ not: { slot: 'a', set: true } }), 'not');
	assert.equal(condKind({ slot: 'a', eq: 1 }), 'leaf');
	assert.equal(condKind({}), 'leaf');
});

test('bind sources round trip between the form and the spec', () => {
	assert.equal(bindSource('state', 'verified.customer_id'), 'state.verified.customer_id');
	assert.equal(bindSource('route', 'customer'), 'route.customer');
	assert.deepEqual(bindSource('const', 'produktdoku'), { const: 'produktdoku' });
	assert.deepEqual(parseBindSource('state.verified.customer_id'), { kind: 'state', value: 'verified.customer_id' });
	assert.deepEqual(parseBindSource('route.customer'), { kind: 'route', value: 'customer' });
	assert.deepEqual(parseBindSource({ const: 7 }), { kind: 'const', value: '7' });
	assert.deepEqual(parseBindSource(undefined), { kind: 'const', value: '' });
	assert.equal(describeBindSource({ const: 'x' }), 'x');
	assert.equal(describeBindSource('route.a'), 'route.a');
});

test('a constant bind keeps a number a number', () => {
	assert.deepEqual(bindSource('const', '12'), { const: 12 });
	assert.deepEqual(bindSource('const', 'true'), { const: true });
	assert.deepEqual(bindSource('const', 'doc-1'), { const: 'doc-1' });
});

test('a gate value is read as the slot type says', () => {
	assert.equal(slotValueFromText('integer', '3'), 3);
	assert.equal(slotValueFromText('number', '2.5'), 2.5);
	assert.equal(slotValueFromText('boolean', 'true'), true);
	assert.equal(slotValueFromText('boolean', 'false'), false);
	assert.equal(slotValueFromText('enum', 'billing'), 'billing');
	assert.equal(slotValueFromText('integer', 'abc'), 'abc', 'left as typed so the validator can say why');
});

test('lists are typed comma or line separated', () => {
	assert.deepEqual(splitList('a, b\n c ,,'), ['a', 'b', 'c']);
	assert.deepEqual(splitList('  '), []);
});

test('a test turn is labelled by how it ended', () => {
	assert.equal(testTurnLabel('completed'), 'agents-test-status-completed');
	assert.equal(testTurnLabel('errored'), 'agents-test-status-errored');
	assert.equal(testTurnLabel('suspended'), 'agents-test-status-suspended');
	assert.equal(testTurnLabel('whatever'), 'agents-test-status-other');
});

test('a paused test turn says what it waits for', () => {
	assert.equal(suspensionLabel('secure_input'), 'agents-test-waiting-secure-input');
	assert.equal(suspensionLabel('approval'), 'agents-test-waiting-approval');
	assert.equal(suspensionLabel('human_answer'), 'agents-test-waiting-human');
});

test('renaming a key keeps its place and refuses a taken name', () => {
	assert.deepEqual(Object.keys(renameKey({ a: 1, b: 2, c: 3 }, 'b', 'x')), ['a', 'x', 'c']);
	assert.deepEqual(renameKey({ a: 1, b: 2 }, 'a', 'b'), { a: 1, b: 2 });
	assert.deepEqual(renameKey({ a: 1 }, 'zz', 'y'), { a: 1 });
});

test('renaming or removing a route keeps the router order naming routes that exist', () => {
	const spec = {
		routes: { billing: { agent: 'a' }, staff: { human: {} } },
		router: { kind: 'rules', order: ['billing', 'staff'] }
	};
	assert.equal(renameRoute(spec, 'staff', 'people'), 'people');
	assert.deepEqual(Object.keys(spec.routes), ['billing', 'people']);
	assert.deepEqual(spec.router.order, ['billing', 'people']);
	removeRoute(spec, 'billing');
	assert.deepEqual(spec.router.order, ['people']);
	removeRoute(spec, 'people');
	assert.deepEqual(spec.router, { kind: 'rules' }, 'an empty order is no order');
	assert.deepEqual(spec.routes, {});
});

test('a fresh name skips the ones in use', () => {
	assert.equal(freshName({}, 'slot'), 'slot_1');
	assert.equal(freshName({ slot_1: 1, slot_2: 2 }, 'slot'), 'slot_3');
});

test('a slot may be written by the model, the host or a declared verifier', () => {
	assert.deepEqual(writerOptions({}), ['llm', 'host']);
	assert.deepEqual(writerOptions({ verifiers: { otp: {} } }), ['llm', 'host', 'verifier:otp']);
});

test('slot infos tell the gate editor each slot type, values and writers', () => {
	assert.deepEqual(
		slotInfos({
			state: {
				issue: { type: 'enum', values: ['billing', 'technical'], set_by: ['llm'] },
				verified: { type: 'subject', set_by: ['verifier:otp', 'host'] }
			}
		}),
		[
			{ name: 'issue', type: 'enum', values: ['billing', 'technical'], writers: ['llm'] },
			{ name: 'verified', type: 'subject', values: [], writers: ['verifier:otp', 'host'] }
		]
	);
	assert.deepEqual(slotInfos({}), []);
});

test('a reactive proxy can be cleaned and shaped like a plain object', () => {
	const proxy = new Proxy({ main: { pool: 'p', tools: [] } }, {});
	assert.deepEqual(cleanSpec(proxy), { main: { pool: 'p' } });
	assert.deepEqual(ensureShape(proxy).main.pool, 'p');
});
