import assert from 'node:assert/strict';
import test from 'node:test';

import {
	describeCond,
	edgeOnPath,
	edgePath,
	issuesByNode,
	layoutCanvas,
	nodeForPath,
	summarizeMain,
	targetKindOf,
	testPath
} from './agent-canvas.ts';
import {
	addRoute,
	cleanSpec,
	ensureShape,
	freshName,
	removeRoute,
	renameKey,
	renameRoute,
	type Spec,
	type TestDebug
} from './agents.ts';

const spec = (): Spec =>
	ensureShape({
		main: { pool: 'chat', tools: ['rag_search'], skills: ['brand'] },
		state: { issue: { type: 'enum', values: ['billing'], set_by: ['llm'] } },
		routes: {
			billing: { when: { slot: 'issue', eq: 'billing' }, agent: 'a1', task: 't' },
			human: { when: { slot: 'issue', set: true }, human: { inbox: 'support' } },
			peer: { when: { slot: 'issue', set: true }, a2a: { url: 'https://x' } },
			loopy: { when: { slot: 'issue', set: true }, loop: { max: 3 }, description: 'd' }
		}
	});

test('layout has main plus gate, route and target per route, with edges main > gate > route > target', () => {
	const g = layoutCanvas(spec());
	assert.equal(g.nodes.length, 1 + 4 * 3);
	assert.equal(g.edges.length, 4 * 3);
	assert.deepEqual(
		g.edges.filter((e) => e.route === 'billing').map((e) => [e.from, e.to]),
		[
			['main', 'gate:billing'],
			['gate:billing', 'route:billing'],
			['route:billing', 'target:billing']
		]
	);
});

test('layout is deterministic and lays columns left to right, rows top to bottom in spec order', () => {
	const s = spec();
	assert.deepEqual(layoutCanvas(s), layoutCanvas(structuredClone(s)));
	const g = layoutCanvas(s);
	const at = (id: string) => g.nodes.find((n) => n.id === id)!;
	assert.ok(at('main').x < at('gate:billing').x);
	assert.ok(at('gate:billing').x < at('route:billing').x);
	assert.ok(at('route:billing').x < at('target:billing').x);
	assert.equal(at('gate:billing').x, at('gate:peer').x);
	assert.ok(at('gate:billing').y < at('gate:human').y);
	assert.ok(at('gate:human').y < at('gate:peer').y);
});

test('no node overlaps another, and all fit the canvas', () => {
	const g = layoutCanvas(spec());
	for (const a of g.nodes) {
		assert.ok(a.x >= 0 && a.y >= 0 && a.x + a.w <= g.width && a.y + a.h <= g.height, a.id);
		for (const b of g.nodes) {
			if (a === b) continue;
			const apart = a.x + a.w <= b.x || b.x + b.w <= a.x || a.y + a.h <= b.y || b.y + b.h <= a.y;
			assert.ok(apart, `${a.id} overlaps ${b.id}`);
		}
	}
});

test('main is vertically centred; a spec without routes still lays out main alone', () => {
	const g = layoutCanvas(ensureShape({}));
	assert.equal(g.nodes.length, 1);
	assert.equal(g.edges.length, 0);
	const main = g.nodes[0];
	assert.equal(main.y * 2 + main.h, g.height);
});

test('target kinds: agent, human, and unknown kinds rendered by their key', () => {
	const s = spec();
	assert.equal(targetKindOf(s.routes.billing), 'agent');
	assert.equal(targetKindOf(s.routes.human), 'human');
	assert.equal(targetKindOf(s.routes.peer), 'a2a');
	assert.equal(targetKindOf(s.routes.loopy), 'loop');
	assert.equal(targetKindOf({ when: {}, agent: '' }), 'agent');
	const kinds = layoutCanvas(s).nodes.filter((n) => n.kind === 'target').map((n) => n.targetKind);
	assert.deepEqual(kinds, ['agent', 'human', 'a2a', 'loop']);
});

test('edgePath runs from the right edge of the source to the left edge of the target', () => {
	const g = layoutCanvas(spec());
	const from = g.nodes.find((n) => n.id === 'main')!;
	const to = g.nodes.find((n) => n.id === 'gate:billing')!;
	assert.match(edgePath(from, to), new RegExp(`^M${from.x + from.w} ${from.y + from.h / 2} C.* ${to.x} ${to.y + to.h / 2}$`));
});

test('server issue paths map to the node that shows them', () => {
	const s = spec();
	const cases: [string, string | null][] = [
		['main.tools[0]', 'main'],
		['main.pool', 'main'],
		['state.issue.values', 'main'],
		['router.order', 'main'],
		['routes.billing', 'route:billing'],
		['routes.billing.description', 'route:billing'],
		['routes.billing.when.all[1].slot', 'gate:billing'],
		['routes.billing.when', 'gate:billing'],
		['routes.billing.agent', 'target:billing'],
		['routes.billing.task', 'target:billing'],
		['routes.billing.bind.customer', 'target:billing'],
		['routes.human.human.notify', 'target:human'],
		['routes.peer.a2a.url', 'target:peer'],
		['routes.loopy.loop', 'target:loopy'],
		['routes.billing.surprise', 'route:billing'],
		['publish.origins[0]', null],
		['finish.schema', null],
		['', null]
	];
	for (const [path, node] of cases) assert.equal(nodeForPath(s, path), node, path);
});

test('issuesByNode groups the issues and ignores those no node shows', () => {
	const by = issuesByNode(spec(), [
		{ path: 'main.pool', message: 'a' },
		{ path: 'main.tools[0]', message: 'b' },
		{ path: 'routes.billing.when.slot', message: 'c' },
		{ path: 'publish.origins', message: 'd' }
	]);
	assert.equal(by.get('main')?.length, 2);
	assert.equal(by.get('gate:billing')?.[0].message, 'c');
	assert.equal(by.size, 2);
});

test('verifiers are summarised by id and kind', () => {
	assert.deepEqual(summarizeMain({ verifiers: { otp: { kind: 'mcp_code' }, x: {} } }).verifiers, [
		{ id: 'otp', kind: 'mcp_code' },
		{ id: 'x', kind: '' }
	]);
});

test('summaries and gate descriptions', () => {
	assert.deepEqual(summarizeMain(spec()), { pool: 'chat', tools: 1, skills: 1, slots: ['issue'], verifiers: [] });
	assert.deepEqual(summarizeMain({}), { pool: '', tools: 0, skills: 0, slots: [], verifiers: [] });
	assert.equal(describeCond({ slot: 'issue', eq: 'billing' }, 'set'), 'issue = billing');
	assert.equal(describeCond({ slot: 'issue', in: ['a', 'b'] }, 'set'), 'issue ∈ [a, b]');
	assert.equal(describeCond({ slot: 'v', provenance: 'verifier:otp', max_age: '15m' }, 'is set'), 'v is set @verifier:otp ≤ 15m');
	assert.equal(
		describeCond({ all: [{ slot: 'a', set: true }, { any: [{ slot: 'b', eq: 1 }, { not: { slot: 'c', set: true } }] }] }, 'set'),
		'a set & (b = 1 | !c set)'
	);
	assert.equal(describeCond(undefined, 'set'), '');
});

const debug: TestDebug = {
	slots: [],
	routes: [
		{ route: 'billing', open: true, missing: [] },
		{ route: 'human', open: false, missing: [] }
	],
	routing: [{ picked: null }, { picked: 'billing' }],
	sub_agents: [{ route: 'billing', outcome: { status: 'completed' } }],
	tool_calls: []
};

test('the last test turn becomes gate status, the picked route and dispatched sub-agents', () => {
	const path = testPath(debug)!;
	assert.deepEqual(path.open, { billing: true, human: false });
	assert.equal(path.picked, 'billing');
	assert.deepEqual(path.dispatched, { billing: 'completed' });
	assert.equal(testPath(null), null);
	assert.equal(testPath({ ...debug, routing: [] })!.picked, null);
});

test('edges on the test path are those of the picked or dispatched route', () => {
	const path = testPath(debug);
	const edges = layoutCanvas(spec()).edges;
	assert.ok(edges.filter((e) => e.route === 'billing').every((e) => edgeOnPath(e, path)));
	assert.ok(edges.filter((e) => e.route !== 'billing').every((e) => !edgeOnPath(e, path)));
	assert.ok(edges.every((e) => !edgeOnPath(e, null)));
});

/* Round trip: the canvas and the form builder drive the same helpers on the same spec. */

test('adding a route on the canvas gives what the form builder gave', () => {
	const viaCanvas = spec();
	const viaForm = spec();
	const name = addRoute(viaCanvas);
	viaForm.routes[freshName(viaForm.routes, 'route')] = { when: { slot: '', set: true }, agent: '', task: '' };
	assert.equal(name, 'route_1');
	assert.deepEqual(cleanSpec(viaCanvas), cleanSpec(viaForm));
	assert.equal(addRoute(viaCanvas), 'route_2');
});

test('removing and renaming a route on the canvas equals the form builder edit and keeps order', () => {
	const viaCanvas = spec();
	const viaForm = spec();
	removeRoute(viaCanvas, 'human');
	delete viaForm.routes.human;
	assert.deepEqual(viaCanvas, viaForm);

	assert.equal(renameRoute(viaCanvas, 'billing', ' invoices '), 'invoices');
	viaForm.routes = renameKey(viaForm.routes, 'billing', 'invoices');
	assert.deepEqual(viaCanvas, viaForm);
	assert.deepEqual(Object.keys(viaCanvas.routes), ['invoices', 'peer', 'loopy']);
});

test('a blank or taken name leaves the route where it is', () => {
	const s = spec();
	assert.equal(renameRoute(s, 'billing', '  '), 'billing');
	assert.equal(renameRoute(s, 'billing', 'human'), 'billing');
	assert.equal(renameRoute(s, 'nope', 'x'), 'nope');
	assert.deepEqual(Object.keys(s.routes), ['billing', 'human', 'peer', 'loopy']);
});

test('a canvas edit survives the JSON tab: stringify, parse, reshape is the same spec', () => {
	const s = spec();
	addRoute(s);
	removeRoute(s, 'peer');
	assert.deepEqual(ensureShape(JSON.parse(JSON.stringify(cleanSpec(s)))), ensureShape(cleanSpec(s)));
});
