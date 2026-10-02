/**
 * The pure half of the agent Tests tab (`docs/agents.md`, "What #99 built"):
 * the wire shapes of `/api/v0/agents/{id}/tests` and `/test-runs`, and the
 * form model that turns a stored case (script plus expectations) into
 * editable fields and back. No framework imports, so `node --test` covers it.
 *
 * What a case means is the server's to judge; this only keeps the form and
 * the stored JSON in step, and says which typed JSON a field did not parse.
 */

export type ScriptStep = { say: string } | { write: { slot: string; value: unknown; writer: string } };

export interface Expect {
	gates?: Record<string, { open: boolean; missing?: string[] }>;
	route?: string | null;
	sub_agents?: { called?: string[]; not_called?: string[] };
	bound?: { route: string; name: string; equals: unknown }[];
	tools?: { called?: string[]; not_called?: string[] };
	answer?: { contains?: string[]; not_contains?: string[] };
	filter?: 'passed' | 'withheld' | 'redacted';
	finished?: boolean;
}

export interface TestCase {
	id: string;
	name: string;
	script: ScriptStep[];
	expect: Expect;
	rubric: string | null;
	created_at: string;
	updated_at: string;
}

export interface CheckResult {
	check: string;
	passed: boolean;
	expected: unknown;
	actual: unknown;
	message: string;
}

export interface Section {
	passed: boolean;
	checks: CheckResult[];
}

export type RubricVerdict = 'passed' | 'failed' | 'error' | 'skipped';

export interface CaseReport {
	passed: boolean;
	error: string | null;
	goal: Section;
	plan: Section;
	action: Section;
	rubric: { verdict: RubricVerdict; reason: string } | null;
	turns: { message: string; status: string; answer: string | null; error: string | null }[];
}

export interface CaseResult {
	case_id: string;
	case_name: string;
	passed: boolean;
	session_id: string | null;
	report: CaseReport;
}

export interface TestRun {
	id: string;
	source: string;
	version: number | null;
	started_by: string;
	started_at: string;
	finished_at: string;
	passed: number;
	failed: number;
	green: boolean;
	rubric?: Record<RubricVerdict, number>;
	results?: CaseResult[];
}

export interface TestsListing {
	cases: TestCase[];
	latest_draft_run: TestRun | null;
	/** The draft and the suite are unchanged since that run. */
	latest_draft_run_current: boolean;
}

export const SECTIONS = ['goal', 'plan', 'action'] as const;
export type SectionName = (typeof SECTIONS)[number];

export const FILTER_OUTCOMES = ['passed', 'withheld', 'redacted'] as const;

/** The writers a script may use besides a verifier; `verifier:<id>` is typed in. */
export const HOST_WRITER = 'host';

export interface StepForm {
	kind: 'say' | 'write';
	text: string;
	slot: string;
	/** JSON when it parses (`{"customer_id": "C-1"}`, `3`), a plain string otherwise. */
	value: string;
	writer: string;
}

export interface GateForm {
	route: string;
	open: boolean;
	/** Comma-separated slot names. */
	missing: string;
}

export interface BoundForm {
	route: string;
	name: string;
	value: string;
}

export interface CaseForm {
	name: string;
	rubric: string;
	steps: StepForm[];
	finished: '' | 'true' | 'false';
	routeMode: '' | 'none' | 'named';
	routeName: string;
	filter: '' | (typeof FILTER_OUTCOMES)[number];
	gates: GateForm[];
	subCalled: string;
	subNotCalled: string;
	toolCalled: string;
	toolNotCalled: string;
	contains: string;
	notContains: string;
	bound: BoundForm[];
}

export const blankStep = (kind: 'say' | 'write' = 'say'): StepForm => ({
	kind,
	text: '',
	slot: '',
	value: '',
	writer: HOST_WRITER
});

export const blankCase = (): CaseForm => ({
	name: '',
	rubric: '',
	steps: [blankStep()],
	finished: 'true',
	routeMode: '',
	routeName: '',
	filter: '',
	gates: [],
	subCalled: '',
	subNotCalled: '',
	toolCalled: '',
	toolNotCalled: '',
	contains: '',
	notContains: '',
	bound: []
});

const list = (items: string[] | undefined) => (items ?? []).join(', ');
const lines = (items: string[] | undefined) => (items ?? []).join('\n');
const text = (v: unknown) => (typeof v === 'string' ? v : JSON.stringify(v));

export function formFromCase(c: TestCase): CaseForm {
	const e = c.expect ?? {};
	return {
		name: c.name,
		rubric: c.rubric ?? '',
		steps: c.script.map((s): StepForm =>
			'say' in s
				? { ...blankStep('say'), text: s.say }
				: { ...blankStep('write'), slot: s.write.slot, value: text(s.write.value), writer: s.write.writer }
		),
		finished: e.finished === undefined ? '' : e.finished ? 'true' : 'false',
		routeMode: e.route === undefined ? '' : e.route === null ? 'none' : 'named',
		routeName: typeof e.route === 'string' ? e.route : '',
		filter: e.filter ?? '',
		gates: Object.entries(e.gates ?? {}).map(([route, g]) => ({
			route,
			open: g.open,
			missing: list(g.missing)
		})),
		subCalled: list(e.sub_agents?.called),
		subNotCalled: list(e.sub_agents?.not_called),
		toolCalled: list(e.tools?.called),
		toolNotCalled: list(e.tools?.not_called),
		contains: lines(e.answer?.contains),
		notContains: lines(e.answer?.not_contains),
		bound: (e.bound ?? []).map((b) => ({ route: b.route, name: b.name, value: text(b.equals) }))
	};
}

const split = (s: string, separator: RegExp) =>
	s
		.split(separator)
		.map((x) => x.trim())
		.filter(Boolean);

/** A typed value: JSON when it parses, the text itself otherwise. */
export function parseValue(raw: string): unknown {
	try {
		return JSON.parse(raw);
	} catch {
		return raw;
	}
}

export interface CaseBody {
	name: string;
	script: ScriptStep[];
	expect: Expect;
	rubric: string | null;
}

/** The body to send. Whether the case is valid is the server's to say. */
export function caseBody(f: CaseForm): CaseBody {
	const script = f.steps
		.filter((s) => (s.kind === 'say' ? s.text.trim() : s.slot.trim()))
		.map((s): ScriptStep =>
			s.kind === 'say'
				? { say: s.text }
				: { write: { slot: s.slot.trim(), value: parseValue(s.value), writer: s.writer.trim() } }
		);
	const expect: Expect = {};
	const gates = f.gates.filter((g) => g.route.trim());
	if (gates.length) {
		expect.gates = {};
		for (const g of gates) {
			const missing = split(g.missing, /,/);
			expect.gates[g.route.trim()] = g.open || !missing.length ? { open: g.open } : { open: false, missing };
		}
	}
	if (f.routeMode === 'none') expect.route = null;
	if (f.routeMode === 'named' && f.routeName.trim()) expect.route = f.routeName.trim();
	const calls = (called: string, not: string) => {
		const c = split(called, /,/);
		const n = split(not, /,/);
		return c.length || n.length ? { ...(c.length ? { called: c } : {}), ...(n.length ? { not_called: n } : {}) } : undefined;
	};
	const sub = calls(f.subCalled, f.subNotCalled);
	if (sub) expect.sub_agents = sub;
	const tools = calls(f.toolCalled, f.toolNotCalled);
	if (tools) expect.tools = tools;
	const bound = f.bound.filter((b) => b.route.trim() && b.name.trim());
	if (bound.length) expect.bound = bound.map((b) => ({ route: b.route.trim(), name: b.name.trim(), equals: parseValue(b.value) }));
	const contains = split(f.contains, /\n/);
	const notContains = split(f.notContains, /\n/);
	if (contains.length || notContains.length) {
		expect.answer = { ...(contains.length ? { contains } : {}), ...(notContains.length ? { not_contains: notContains } : {}) };
	}
	if (f.filter) expect.filter = f.filter;
	if (f.finished) expect.finished = f.finished === 'true';
	return { name: f.name.trim(), script, expect, rubric: f.rubric.trim() || null };
}

/** `draft` or `version:N`, the `source` of a run. */
export const runSource = (version: number | null): string => (version === null ? 'draft' : `version:${version}`);

/** The checks of a report that did not hold, with the section they belong to. */
export function failedChecks(report: CaseReport): { section: SectionName; check: CheckResult }[] {
	return SECTIONS.flatMap((section) => report[section].checks.filter((c) => !c.passed).map((check) => ({ section, check })));
}

/** How a run reads at a glance: green, failing, or nothing ran. */
export function runState(run: Pick<TestRun, 'passed' | 'failed'>): 'green' | 'failing' | 'empty' {
	if (run.failed > 0) return 'failing';
	return run.passed > 0 ? 'green' : 'empty';
}
