import assert from 'node:assert/strict';
import test from 'node:test';

import {
	blankCase,
	blankStep,
	caseBody,
	failedChecks,
	formFromCase,
	parseValue,
	runSource,
	runState,
	type CaseReport,
	type TestCase
} from './agent-tests.ts';

const stored: TestCase = {
	id: 'c1',
	name: 'verified customer reaches billing',
	rubric: 'Polite.',
	script: [
		{ say: 'Question about my invoice' },
		{ write: { slot: 'verified', value: { customer_id: 'C-1' }, writer: 'verifier:otp' } },
		{ say: 'I am verified now' }
	],
	expect: {
		gates: { billing: { open: true }, technical: { open: false, missing: ['issue', 'email'] } },
		route: 'billing',
		sub_agents: { called: ['billing'], not_called: ['technical'] },
		bound: [{ route: 'billing', name: 'customer', equals: 'C-1' }],
		tools: { not_called: ['rag_search'] },
		answer: { contains: ['handled'], not_contains: ['refund'] },
		filter: 'passed',
		finished: true
	},
	created_at: '',
	updated_at: ''
};

test('a stored case survives a round trip through the form', () => {
	const body = caseBody(formFromCase(stored));
	assert.deepEqual(body.script, stored.script);
	assert.deepEqual(body.expect, stored.expect);
	assert.equal(body.name, stored.name);
	assert.equal(body.rubric, 'Polite.');
});

test('route null (no route) and an absent route are different expectations', () => {
	const none = formFromCase({ ...stored, expect: { route: null } });
	assert.equal(none.routeMode, 'none');
	assert.deepEqual(caseBody(none).expect, { route: null });
	const unchecked = formFromCase({ ...stored, expect: { finished: false } });
	assert.equal(unchecked.routeMode, '');
	assert.deepEqual(caseBody(unchecked).expect, { finished: false });
});

test('blank rows and unchecked fields are left out of the expectation', () => {
	const form = blankCase();
	form.name = '  hello  ';
	form.steps = [{ ...blankStep('say'), text: 'hi' }, blankStep('say'), blankStep('write')];
	form.finished = '';
	form.gates = [{ route: ' ', open: true, missing: '' }];
	form.contains = 'a\n\n  b \n';
	const body = caseBody(form);
	assert.equal(body.name, 'hello');
	assert.deepEqual(body.script, [{ say: 'hi' }]);
	assert.deepEqual(body.expect, { answer: { contains: ['a', 'b'] } });
	assert.equal(body.rubric, null);
});

test('typed values are JSON when they parse and text otherwise', () => {
	assert.deepEqual(parseValue('{"a": 1}'), { a: 1 });
	assert.equal(parseValue('3'), 3);
	assert.equal(parseValue('technical'), 'technical');
	const form = blankCase();
	form.steps = [
		{ ...blankStep('say'), text: 'hi' },
		{ ...blankStep('write'), slot: 'issue', value: 'technical', writer: ' host ' }
	];
	assert.deepEqual(caseBody(form).script[1], { write: { slot: 'issue', value: 'technical', writer: 'host' } });
});

test('a closed gate keeps the slots it must still miss, an open one drops them', () => {
	const form = blankCase();
	form.gates = [
		{ route: 'billing', open: false, missing: ' verified , ' },
		{ route: 'technical', open: true, missing: 'ignored' }
	];
	assert.deepEqual(caseBody(form).expect.gates, {
		billing: { open: false, missing: ['verified'] },
		technical: { open: true }
	});
});

test('runs read as green, failing or empty, and name their source', () => {
	assert.equal(runState({ passed: 2, failed: 0 }), 'green');
	assert.equal(runState({ passed: 2, failed: 1 }), 'failing');
	assert.equal(runState({ passed: 0, failed: 0 }), 'empty');
	assert.equal(runSource(null), 'draft');
	assert.equal(runSource(3), 'version:3');
});

test('the failed checks of a report carry their section', () => {
	const check = (passed: boolean) => ({ check: 'x', passed, expected: 1, actual: 2, message: 'm' });
	const report: CaseReport = {
		passed: false,
		error: null,
		goal: { passed: true, checks: [check(true)] },
		plan: { passed: false, checks: [check(false), check(true)] },
		action: { passed: false, checks: [check(false)] },
		rubric: null,
		turns: []
	};
	assert.deepEqual(
		failedChecks(report).map((f) => f.section),
		['plan', 'action']
	);
});
