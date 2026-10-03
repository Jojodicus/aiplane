/**
 * The agent builder's data layer: wire types, the typed API calls, and the
 * pure helpers the editor components lean on (`docs/agents.md`).
 *
 * The spec is edited as a plain object that mirrors the stored JSON. The
 * components bind to it directly, so [`ensureShape`] gives every container
 * they bind into a home before editing starts and [`cleanSpec`] takes the
 * blanks back out before it is saved. What the spec means is the server's to
 * judge: nothing here validates, it only keeps the validator's answers
 * attached to the field they are about (by `path`).
 */
import type { ActivityPage, Verification } from './agent-activity.ts';
import type { AgentAnalytics } from './agent-analytics.ts';
import type { CaseBody, TestCase, TestRun, TestsListing } from './agent-tests.ts';
import { ApiError, request } from './api.ts';

/* eslint-disable @typescript-eslint/no-explicit-any -- a spec is open-ended JSON */
export type Spec = Record<string, any>;

/** One problem the server found in a spec, at a dotted/indexed path such as `main.tools[0]`. */
export interface SpecIssue {
	path: string;
	message: string;
}

export interface AgentSummary {
	id: string;
	name: string;
	display: string;
	description: string;
	created_by: string;
	created_at: string;
	updated_at: string;
	disabled_at: string | null;
	/** `null` until the agent was published once. */
	live_version: number | null;
	access: 'read' | 'write';
}

export interface Grant {
	kind: GrantKind;
	ref: string;
	granted_by: string;
	granted_at: string;
}

export type GrantKind = 'pool' | 'tool' | 'connector' | 'skill' | 'rag_collection';
export const GRANT_KINDS: GrantKind[] = ['pool', 'tool', 'connector', 'skill', 'rag_collection'];

export interface Share {
	subject_kind: 'user' | 'group';
	subject_id: string;
	access: 'read' | 'write';
}

/** Someone who may answer the agent's inbox items without a share (#96). */
export interface Responder {
	subject_kind: 'user' | 'group';
	subject_id: string;
	added_by: string;
	added_at: string;
}

export type ChannelKind = 'slack' | 'discord';
export const CHANNEL_KINDS: ChannelKind[] = ['slack', 'discord'];

/** A Slack or Discord incoming webhook the agent's waiting turns are announced on. The URL is never read back. */
export interface NotifyChannel {
	id: string;
	kind: ChannelKind;
	name: string;
	url_host: string;
	details: boolean;
	lang: string;
	created_by: string;
	created_at: string;
}

/** A key a website embeds the agent with (`docs/embed.md`). The key itself is shown once, when it is created. */
export interface EmbedKey {
	id: string;
	name: string;
	origins: string[];
	created_by: string;
	created_at: string;
	revoked_at: string | null;
}

/** The script tag a website owner pastes before `</body>`. */
export function embedSnippet(scriptUrl: string, key: string): string {
	return `<script src="${scriptUrl}" data-agent-key="${key}" async></script>`;
}

export interface NewChannel {
	kind: ChannelKind;
	name: string;
	url: string;
	details: boolean;
	lang: string;
}

export interface AuditEntry {
	kind: string;
	actor_id: string | null;
	detail: unknown;
	created_at: string;
}

export interface AgentDetail extends AgentSummary {
	draft_spec: Spec;
	live_spec: Spec | null;
	publish_issues: SpecIssue[];
	grants: Grant[];
	shares: Share[];
	audit: AuditEntry[];
}

export interface AgentVersion {
	version: number;
	spec: Spec;
	published_by: string;
	published_at: string;
}

/** What the signed-in manager holds, and so may grant (`GET /api/v0/agent-resources`). */
export interface AgentResources {
	pools: string[];
	tools: { id: string; name: string; description: string | null }[];
	connectors: { key: string; name: string; tools: string[] }[];
	skills: string[];
	rag_collections: { id: number; name: string }[];
	/** The pool an admin mapped to each of the setup's model choices, held by the caller or not. */
	tiers?: { fast: string | null; balanced: string | null; thorough: string | null };
}

export interface Unmet {
	path: string;
	slot?: string;
	kind: string;
	message: string;
}

export interface SlotDebug {
	slot: string;
	status: 'set' | 'missing' | 'invalid';
	reason?: string;
	value?: unknown;
	provenance?: string;
	set_at?: string;
	set_by: string[];
}

export interface RouteDebug {
	route: string;
	description?: string;
	open: boolean;
	missing: Unmet[];
}

/** The topic guard's decision on a turn's message, under a strict scope. */
export interface ScopeDebug {
	verdict: 'in_scope' | 'out_of_scope' | 'failed';
	topics: string[];
	/** Why the guard could not decide; the message was refused all the same. */
	error?: string;
}

export interface TestDebug {
	slots: SlotDebug[];
	routes: RouteDebug[];
	routing: { routes?: unknown; picked: string | null; reason?: string }[];
	sub_agents: {
		route?: string;
		sub_agent?: string;
		sub_agent_id?: string;
		/** An `a2a` route's external agent, by its card's name and URL. */
		remote_agent?: string | null;
		card_url?: string;
		/** A `loop` route's child run: which iteration, worker or critic. */
		loop?: { route: string; iteration: number; role: 'worker' | 'critic' };
		version?: number;
		outcome?: { status: string; result?: unknown; reason?: unknown; summary?: string };
	}[];
	tool_calls: { tool: string; decision: string; policy: string }[];
	loops?: { event: 'loop_iteration' | 'loop_finished'; route: string; iteration?: number; iterations?: number; accepted: boolean; feedback?: string; stopped?: string }[];
	/** `null` unless the draft's scope is strict. */
	scope?: ScopeDebug | null;
}

/** What a suspended turn waits for, as a manager sees it. */
export interface Suspension {
	request_id: string;
	kind: 'approval' | 'secure_input' | 'human_answer';
	message?: string;
	tool_call_id?: string;
	tool?: string;
	options: ('allow_once' | 'deny' | 'value')[];
	expires_at: string;
}

export interface TestTurn {
	session_id: string;
	turn_id: string;
	status: string;
	answer: string | null;
	error: string | null;
	suspension: Suspension | null;
	draft_version?: number;
	debug?: TestDebug;
}

/** The answer to a suspension: the decision, and the value only `value` carries. */
export type ResumeDecision = { decision: 'allow_once' } | { decision: 'deny' } | { decision: 'value'; value: string };

/* ---- errors --------------------------------------------------------- */

/** A failed agent call, with the validator's per-field issues when it was a 422. */
export interface AgentError {
	status: number;
	code?: string;
	message: string;
	issues: SpecIssue[];
}

/**
 * Read the gateway's error envelope out of a failed call. `request` folds the
 * body into the message after ` — `; the envelope is where the code, the
 * human message and the validator's `issues` live.
 */
export function parseSpecError(err: unknown): AgentError {
	if (!(err instanceof ApiError)) {
		return { status: 0, message: err instanceof Error ? err.message : String(err), issues: [] };
	}
	const at = err.message.indexOf(' — ');
	const detail = at >= 0 ? err.message.slice(at + 3) : '';
	try {
		const envelope = JSON.parse(detail)?.error;
		if (envelope && typeof envelope === 'object') {
			return {
				status: err.status,
				code: typeof envelope.code === 'string' ? envelope.code : err.code,
				message: typeof envelope.message === 'string' ? envelope.message : err.message,
				issues: Array.isArray(envelope.issues) ? envelope.issues : []
			};
		}
	} catch {
		// not an envelope; fall through to the raw message
	}
	return { status: err.status, code: err.code, message: err.message, issues: [] };
}

async function call<T>(path: string, init?: RequestInit): Promise<T> {
	try {
		return await request<T>(path, init);
	} catch (err) {
		throw parseSpecError(err);
	}
}

const json = (method: string, body?: unknown): RequestInit => ({
	method,
	headers: { 'content-type': 'application/json' },
	body: body === undefined ? undefined : JSON.stringify(body)
});

/** Every call rejects with an [`AgentError`]. */
export const agentsApi = {
	list: () => call<{ agents: AgentSummary[] }>('/api/v0/agents').then((r) => r.agents),
	create: (body: { name: string; display?: string; description?: string; spec?: Spec }) =>
		call<{ agent: AgentSummary }>('/api/v0/agents', json('POST', body)).then((r) => r.agent),
	get: (id: string) => call<{ agent: AgentDetail }>(`/api/v0/agents/${id}`).then((r) => r.agent),
	saveDraft: (id: string, spec: Spec) =>
		call<{ draft_spec: Spec; live_version: number | null }>(`/api/v0/agents/${id}/draft`, json('PUT', { spec })),
	publish: (id: string) =>
		call<{ version: number; live_version: number }>(`/api/v0/agents/${id}/publish`, json('POST', {})),
	versions: (id: string) =>
		call<{ live_version: number | null; versions: AgentVersion[] }>(`/api/v0/agents/${id}/versions`),
	setLive: (id: string, version: number) =>
		call<{ live_version: number }>(`/api/v0/agents/${id}/live`, json('POST', { version })),
	analytics: (id: string, query: string) =>
		call<AgentAnalytics>(`/api/v0/agents/${id}/analytics?${query}`),
	activity: (id: string, query: string) => call<ActivityPage>(`/api/v0/agents/${id}/activity?${query}`),
	verifyActivity: (id: string) => call<Verification>(`/api/v0/agents/${id}/activity/verify`),
	tests: (id: string) => call<TestsListing>(`/api/v0/agents/${id}/tests`),
	createTest: (id: string, body: CaseBody) => call<{ case: TestCase }>(`/api/v0/agents/${id}/tests`, json('POST', body)),
	updateTest: (id: string, caseId: string, body: CaseBody) =>
		call<{ case: TestCase }>(`/api/v0/agents/${id}/tests/${caseId}`, json('PUT', body)),
	deleteTest: (id: string, caseId: string) => call<void>(`/api/v0/agents/${id}/tests/${caseId}`, json('DELETE')),
	runTests: (id: string, source: string) => call<TestRun>(`/api/v0/agents/${id}/tests/run`, json('POST', { source })),
	testRuns: (id: string) => call<{ runs: TestRun[] }>(`/api/v0/agents/${id}/test-runs`),
	testRun: (id: string, runId: string) => call<TestRun>(`/api/v0/agents/${id}/test-runs/${runId}`),
	remove: (id: string) => call<void>(`/api/v0/agents/${id}`, json('DELETE')),
	share: (id: string, share: Share) => call<Share>(`/api/v0/agents/${id}/shares`, json('POST', share)),
	revokeShare: (id: string, share: Pick<Share, 'subject_kind' | 'subject_id'>) =>
		call<void>(`/api/v0/agents/${id}/shares/revoke`, json('POST', share)),
	responders: (id: string) =>
		call<{ responders: Responder[] }>(`/api/v0/agents/${id}/responders`).then((r) => r.responders),
	addResponder: (id: string, r: Pick<Responder, 'subject_kind' | 'subject_id'>) =>
		call<unknown>(`/api/v0/agents/${id}/responders`, json('POST', r)),
	removeResponder: (id: string, r: Pick<Responder, 'subject_kind' | 'subject_id'>) =>
		call<void>(`/api/v0/agents/${id}/responders/revoke`, json('POST', r)),
	channels: (id: string) =>
		call<{ channels: NotifyChannel[] }>(`/api/v0/agents/${id}/channels`).then((r) => r.channels),
	addChannel: (id: string, channel: NewChannel) =>
		call<{ channel: NotifyChannel }>(`/api/v0/agents/${id}/channels`, json('POST', channel)).then((r) => r.channel),
	removeChannel: (id: string, channelId: string) =>
		call<void>(`/api/v0/agents/${id}/channels/${channelId}`, json('DELETE')),
	embedKeys: (id: string) =>
		call<{ embed_keys: EmbedKey[] }>(`/api/v0/agents/${id}/embed-keys`).then((r) => r.embed_keys),
	createEmbedKey: (id: string, body: { name: string; origins: string[] }) =>
		call<{ embed_key: EmbedKey; key: string }>(`/api/v0/agents/${id}/embed-keys`, json('POST', body)),
	revokeEmbedKey: (id: string, keyId: string) =>
		call<void>(`/api/v0/agents/${id}/embed-keys/${keyId}/revoke`, json('POST')),
	grant: (id: string, kind: GrantKind, ref: string) =>
		call<{ added: boolean }>(`/api/v0/system-principals/${id}/grants`, json('POST', { kind, ref })),
	revokeGrant: (id: string, kind: GrantKind, ref: string) =>
		call<void>(`/api/v0/system-principals/${id}/grants/revoke`, json('POST', { kind, ref })),
	resources: () => call<AgentResources>('/api/v0/agent-resources'),
	testTurn: (id: string, message: string, sessionId: string | null) =>
		call<TestTurn>(
			`/api/v0/agents/${id}/test-turn`,
			json('POST', sessionId ? { message, session_id: sessionId } : { message })
		),
	resumeTurn: (id: string, sessionId: string, turnId: string, requestId: string, answer: ResumeDecision) =>
		call<TestTurn>(
			`/api/v0/agents/${id}/conversations/${sessionId}/turns/${turnId}/resume`,
			json('POST', { ...answer, request_id: requestId })
		)
};

/* ---- issues, by path ------------------------------------------------ */

/** The messages for exactly `path`. */
export function issuesAt(issues: SpecIssue[], path: string): string[] {
	return issues.filter((i) => i.path === path).map((i) => i.message);
}

/** The issues at `path` or anywhere below it (`path.x`, `path[0]`). */
export function issuesUnder(issues: SpecIssue[], path: string): SpecIssue[] {
	return issues.filter(
		(i) => i.path === path || i.path.startsWith(`${path}.`) || i.path.startsWith(`${path}[`)
	);
}

/**
 * The issues of the last refused save that still point into `spec`. One
 * under an entry of a named map (a route, slot, verifier, pattern, …) or a
 * list item the manager has since removed is dropped; one naming a key
 * missing from an entry that still exists (`state.issue.values`) is kept,
 * since that is what it asks to add.
 */
export function liveIssues(issues: SpecIssue[], spec: Spec): SpecIssue[] {
	return issues.filter((issue) => {
		const parts = issue.path.match(/[^.[\]]+/g) ?? [];
		let node: unknown = spec;
		for (const [i, part] of parts.entries()) {
			const container = node as Record<string, unknown> | null;
			if (container !== null && typeof container === 'object' && part in container) {
				node = container[part];
				continue;
			}
			return i === parts.length - 1 && !/^\d+$/.test(part) && !NAMED_MAPS.has(parts[i - 1]);
		}
		return true;
	});
}

/* ---- the spec as an editing buffer ---------------------------------- */

/** A deep copy that also accepts a Svelte `$state` proxy, which `structuredClone` refuses. */
const clone = <T>(value: T): T => JSON.parse(JSON.stringify(value));

/** `spec` with every container the editor binds into present. Unknown keys are kept. */
export function ensureShape(spec: Spec): Spec {
	const out: Spec = clone(spec ?? {});
	const main = (out.main ??= {});
	const instructions = (main.instructions ??= {});
	instructions.orchestration ??= '';
	instructions.response ??= '';
	main.tools ??= [];
	main.skills ??= [];
	main.tool_resources ??= {};
	main.budget ??= {};
	out.state ??= {};
	out.routes ??= {};
	return out;
}

/** Containers whose entries are named: an empty entry is a mistake to report, not noise to drop. */
const NAMED_MAPS = new Set(['state', 'verifiers', 'routes', 'tool_resources', 'patterns']);
/** Subtrees the editor treats as opaque JSON: never pruned. */
const OPAQUE = new Set(['when', 'schema', 'human']);

const blank = (v: unknown): boolean =>
	v === undefined ||
	v === null ||
	v === '' ||
	(typeof v === 'number' && Number.isNaN(v)) ||
	(Array.isArray(v) && v.length === 0) ||
	(typeof v === 'object' && !Array.isArray(v) && Object.keys(v as object).length === 0);

function prune(value: unknown, key: string): unknown {
	if (Array.isArray(value)) return value.map((v) => prune(v, '')).filter((v) => !blank(v));
	if (value && typeof value === 'object') {
		const out: Record<string, unknown> = {};
		for (const [k, v] of Object.entries(value)) {
			if (OPAQUE.has(k)) {
				out[k] = v;
				continue;
			}
			const kept = prune(v, k);
			if (!blank(kept) || (NAMED_MAPS.has(key) && v && typeof v === 'object')) out[k] = kept;
		}
		return out;
	}
	return value;
}

/** The spec to save: blanks removed, `main` always present, input untouched. */
export function cleanSpec(spec: Spec): Spec {
	const pruned = prune(clone(spec), '') as Spec;
	return { ...pruned, main: pruned.main ?? {} };
}

/* ---- gates ---------------------------------------------------------- */

export type CondKind = 'all' | 'any' | 'not' | 'leaf';

export function condKind(cond: Record<string, unknown>): CondKind {
	if ('all' in cond) return 'all';
	if ('any' in cond) return 'any';
	if ('not' in cond) return 'not';
	return 'leaf';
}

/** Read a leaf's typed text as the slot's type says; left as typed when it does not parse. */
export function slotValueFromText(type: string, text: string): unknown {
	if (type === 'integer' || type === 'number') {
		const n = Number(text);
		return text.trim() !== '' && Number.isFinite(n) ? n : text;
	}
	if (type === 'boolean') return text === 'true' ? true : text === 'false' ? false : text;
	return text;
}

/** A comma- or line-separated list, trimmed, without blanks. */
export function splitList(text: string): string[] {
	return text
		.split(/[,\n]/)
		.map((s) => s.trim())
		.filter(Boolean);
}

/* ---- binds ---------------------------------------------------------- */

export type BindKind = 'state' | 'route' | 'const';

/** The spec form of a bind source: `state.<path>`, `route.<name>` or `{const}`. */
export function bindSource(kind: BindKind, value: string): string | { const: unknown } {
	if (kind === 'state') return `state.${value}`;
	if (kind === 'route') return `route.${value}`;
	const n = Number(value);
	if (value.trim() !== '' && Number.isFinite(n)) return { const: n };
	if (value === 'true' || value === 'false') return { const: value === 'true' };
	return { const: value };
}

export function parseBindSource(source: unknown): { kind: BindKind; value: string } {
	if (typeof source === 'string') {
		if (source.startsWith('state.')) return { kind: 'state', value: source.slice(6) };
		if (source.startsWith('route.')) return { kind: 'route', value: source.slice(6) };
		return { kind: 'const', value: source };
	}
	if (source && typeof source === 'object' && 'const' in source) {
		return { kind: 'const', value: String((source as { const: unknown }).const) };
	}
	return { kind: 'const', value: '' };
}

export function describeBindSource(source: unknown): string {
	const { kind, value } = parseBindSource(source);
	return kind === 'const' ? value : `${kind}.${value}`;
}

/* ---- the test chat -------------------------------------------------- */

/** The Fluent key that says what a suspended test turn waits for. */
export function suspensionLabel(kind: Suspension['kind']): string {
	switch (kind) {
		case 'secure_input':
			return 'agents-test-waiting-secure-input';
		case 'approval':
			return 'agents-test-waiting-approval';
		default:
			return 'agents-test-waiting-human';
	}
}

/**
 * How the test chat takes a pause's `value`: a code the visitor would type is
 * masked, a staff member's answer to a handoff is plain text they read back,
 * labelled as in the inbox.
 */
export function answerField(kind: Suspension['kind']): { secret: boolean; label: string; submit: string } {
	return kind === 'human_answer'
		? { secret: false, label: 'inbox-answer-label', submit: 'inbox-send-answer' }
		: { secret: true, label: 'agents-test-value-label', submit: 'agents-test-answer' };
}

export function testTurnLabel(status: string): string {
	switch (status) {
		case 'completed':
			return 'agents-test-status-completed';
		case 'errored':
			return 'agents-test-status-errored';
		case 'suspended':
			return 'agents-test-status-suspended';
		default:
			return 'agents-test-status-other';
	}
}

/* ---- named maps ----------------------------------------------------- */

/** `map` with key `from` renamed to `to`, keeping its position. Unchanged when `to` is taken. */
export function renameKey<T>(map: Record<string, T>, from: string, to: string): Record<string, T> {
	if (from === to || to in map || !(from in map)) return map;
	return Object.fromEntries(Object.entries(map).map(([k, v]) => [k === from ? to : k, v]));
}

/** The first `<prefix>_<n>` not yet a key of `map`. */
export function freshName(map: Record<string, unknown>, prefix: string): string {
	for (let n = 1; ; n++) if (!(`${prefix}_${n}` in map)) return `${prefix}_${n}`;
}

/** Writers a slot can name: the model, the host, and one per declared verifier. */
export function writerOptions(spec: Spec): string[] {
	return ['llm', 'host', ...Object.keys(spec.verifiers ?? {}).map((id) => `verifier:${id}`)];
}

/** What a gate or bind editor needs to know about each declared slot. */
export interface SlotInfo {
	name: string;
	type: string;
	values: string[];
	writers: string[];
}

export function slotInfos(spec: Spec): SlotInfo[] {
	return Object.entries((spec.state ?? {}) as Record<string, Spec>).map(([name, def]) => ({
		name,
		type: def?.type ?? 'string',
		values: Array.isArray(def?.values) ? def.values.map(String) : [],
		writers: Array.isArray(def?.set_by) ? def.set_by.map(String) : []
	}));
}

/* ---- routes ---------------------------------------------------------- */

/** A route as the form builder and the canvas both create it: a gate on no slot yet, and no sub-agent chosen. */
export const newRoute = (): Spec => ({ when: { slot: '', set: true }, agent: '', task: '' });

/** Adds a fresh route to `spec.routes` and returns its name. */
export function addRoute(spec: Spec): string {
	spec.routes ??= {};
	const name = freshName(spec.routes, 'route');
	spec.routes[name] = newRoute();
	return name;
}

/** Removes route `name`, and its place in `router.order`: the server refuses an order naming a route that does not exist. */
export function removeRoute(spec: Spec, name: string): void {
	delete spec.routes?.[name];
	setRouterOrder(spec, (order) => order.filter((r) => r !== name));
}

/** Renames route `from` to `to` (trimmed), keeping its position, in `router.order` too. A blank or taken name changes nothing. Returns the name the route has afterwards. */
export function renameRoute(spec: Spec, from: string, to: string): string {
	const name = to.trim();
	if (!name || name === from || name in spec.routes || !(from in spec.routes)) return from;
	spec.routes = renameKey(spec.routes, from, name);
	setRouterOrder(spec, (order) => order.map((r) => (r === from ? name : r)));
	return name;
}

function setRouterOrder(spec: Spec, edit: (order: string[]) => string[]): void {
	if (!Array.isArray(spec.router?.order)) return;
	const order = edit(spec.router.order);
	if (order.length) spec.router.order = order;
	else delete spec.router.order;
}

/** What the agent's principal has been granted, for the editor's pickers. */
export interface Granted {
	pools: string[];
	tools: { id: string; name: string }[];
	skills: string[];
}
