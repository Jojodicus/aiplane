/**
 * The agent setup assistant's pure half (`docs/ui.md` → "Agent setup"): how
 * each step reads its plain-language model out of the spec and writes it
 * back, the starter templates, the pre-publish checklist and the section
 * status.
 *
 * The spec stays the one source of truth. A step owns a well-defined part of
 * it (the shapes below) and leaves everything else untouched, so the advanced
 * editor and the assistant can edit the same agent in turn: whatever a step
 * does not recognise as its own shows as "set up in the advanced editor" and
 * survives a write. Every `read*` / `write*` pair round-trips, which
 * `agent-setup.test.ts` pins.
 *
 * Text the model reads (tone and language lines, the hand-off task, the
 * descriptions of the slots the assistant manages) is English, like the
 * structured system prompt it ends up in (`docs/agents.md` "What #115
 * built"). Text a person reads comes from the catalogs.
 */
import type { AgentResources, Grant, Spec, SpecIssue } from './agents.ts';
import templates from './agent-templates.json' with { type: 'json' };

/* eslint-disable @typescript-eslint/no-explicit-any -- the spec is open-ended JSON */

const clone = <T>(value: T): T => JSON.parse(JSON.stringify(value));

/* ---- steps ---------------------------------------------------------- */

export const STEPS = ['start', 'basics', 'scope', 'abilities', 'slots', 'identity', 'routes', 'site', 'review'] as const;
export type StepKey = (typeof STEPS)[number];
/** The steps the overview lists as sections: everything but the start and the review. */
export const SECTIONS = STEPS.slice(1, -1) as readonly StepKey[];

export function asStep(value: string | undefined | null): StepKey | null {
	return STEPS.find((s) => s === value) ?? null;
}

/* ---- names ---------------------------------------------------------- */

const MAX_IDENT = 48;

/** A spec identifier (`[a-z][a-z0-9_]*`, ≤ 48) made from a label; `fallback` when nothing usable is left. */
export function identFrom(label: string, fallback: string): string {
	const ascii = label
		.normalize('NFKD')
		.replace(/[̀-ͯ]/g, '')
		.toLowerCase()
		.replace(/ß/g, 'ss')
		.replace(/[^a-z0-9]+/g, '_')
		.replace(/^_+|_+$/g, '');
	const base = /^[a-z]/.test(ascii) ? ascii : ascii ? `${fallback}_${ascii}` : fallback;
	return base.slice(0, MAX_IDENT).replace(/_+$/, '');
}

/** `name`, or `name_2`, `name_3`, … — the first not in `taken`. */
export function unique(name: string, taken: Iterable<string>): string {
	const used = new Set(taken);
	if (!used.has(name)) return name;
	for (let n = 2; ; n++) {
		const candidate = `${name.slice(0, MAX_IDENT - String(n).length - 1)}_${n}`;
		if (!used.has(candidate)) return candidate;
	}
}

/** `customer_number` → `Customer number`, for a slot that has no description. */
export function humanize(key: string): string {
	const words = key.replace(/_/g, ' ').trim();
	return words.charAt(0).toUpperCase() + words.slice(1);
}

/* ---- task & tone ---------------------------------------------------- */

export const TONES = ['friendly', 'factual', 'casual', 'brief', 'detailed', 'formal', 'informal'] as const;
export type Tone = (typeof TONES)[number];

/** The sentence each tone chip stands for in `main.instructions.response`. */
export const TONE_LINES: Record<Tone, string> = {
	friendly: 'Be warm and friendly.',
	factual: 'Stay factual and neutral.',
	casual: 'Keep the tone casual and relaxed.',
	brief: 'Keep answers short and to the point.',
	detailed: 'Give thorough, detailed answers.',
	formal: 'Address the visitor formally.',
	informal: 'Address the visitor informally.'
};

export const ANSWER_LANGUAGES = ['en', 'de', 'fr', 'es', 'ru', 'zh'] as const;
export type AnswerLanguage = (typeof ANSWER_LANGUAGES)[number];
const LANGUAGE_NAMES: Record<AnswerLanguage, string> = {
	en: 'English',
	de: 'German',
	fr: 'French',
	es: 'Spanish',
	ru: 'Russian',
	zh: 'Chinese'
};
const VISITOR_LANGUAGE_LINE = 'Answer in the language the visitor writes in.';
const languageLine = (lang: AnswerLanguage) => `Always answer in ${LANGUAGE_NAMES[lang]}.`;

/** `visitor`: the visitor's language; a code: always that one; `null`: nothing said. */
export type Language = 'visitor' | AnswerLanguage | null;

export interface Basics {
	name: string;
	task: string;
	tones: Tone[];
	language: Language;
	/** Lines of the response instructions no chip stands for, kept as written. */
	extra: string;
	pool: string;
}

export function readBasics(spec: Spec): Basics {
	const lines = String(spec.main?.instructions?.response ?? '').split('\n');
	const tones: Tone[] = [];
	let language: Language = null;
	const extra: string[] = [];
	for (const line of lines) {
		const trimmed = line.trim();
		const tone = TONES.find((t) => TONE_LINES[t] === trimmed);
		const fixed = ANSWER_LANGUAGES.find((l) => languageLine(l) === trimmed);
		if (tone && !tones.includes(tone)) tones.push(tone);
		else if (trimmed === VISITOR_LANGUAGE_LINE && language === null) language = 'visitor';
		else if (fixed && language === null) language = fixed;
		else extra.push(line);
	}
	return {
		name: String(spec.profile?.display ?? ''),
		task: String(spec.main?.instructions?.orchestration ?? ''),
		tones: TONES.filter((t) => tones.includes(t)),
		language,
		extra: extra.join('\n').trim(),
		pool: String(spec.main?.pool ?? '')
	};
}

export function responseText(b: Pick<Basics, 'tones' | 'language' | 'extra'>): string {
	const lines = TONES.filter((t) => b.tones.includes(t)).map((t) => TONE_LINES[t]);
	if (b.language === 'visitor') lines.push(VISITOR_LANGUAGE_LINE);
	else if (b.language) lines.push(languageLine(b.language));
	if (b.extra.trim()) lines.push(b.extra.trim());
	return lines.join('\n');
}

export function writeBasics(spec: Spec, b: Basics): void {
	spec.profile ??= {};
	if (b.name.trim()) spec.profile.display = b.name.trim();
	else delete spec.profile.display;
	spec.main ??= {};
	spec.main.instructions ??= {};
	spec.main.instructions.orchestration = b.task;
	spec.main.instructions.response = responseText(b);
	if (b.pool) spec.main.pool = b.pool;
	else delete spec.main.pool;
}

/* ---- model choice --------------------------------------------------- */

export const TIERS = ['fast', 'balanced', 'thorough'] as const;
export type Tier = (typeof TIERS)[number];
export type Tiers = Record<Tier, string | null>;

/** Which of the admin-mapped choices `pool` is, if any. */
export function tierOf(pool: string, tiers: Tiers | undefined): Tier | null {
	if (!pool || !tiers) return null;
	return TIERS.find((t) => tiers[t] === pool) ?? null;
}

export function hasTiers(tiers: Tiers | undefined): boolean {
	return !!tiers && TIERS.some((t) => !!tiers[t]);
}

/* ---- scope (#115) ---------------------------------------------------- */

export interface Scope {
	topics: string[];
	refusal: string;
	strict: boolean;
}

export function readScope(spec: Spec): Scope {
	const s = spec.scope ?? {};
	return {
		topics: Array.isArray(s.topics) ? s.topics.map(String) : [],
		refusal: String(s.refusal ?? ''),
		strict: s.strict === true
	};
}

export function writeScope(spec: Spec, s: Scope): void {
	const topics = s.topics.map((t) => t.trim()).filter(Boolean);
	const keep = spec.scope?.classifier_pool ? { classifier_pool: spec.scope.classifier_pool } : {};
	if (!topics.length && !s.refusal.trim() && !s.strict && !keep.classifier_pool) {
		delete spec.scope;
		return;
	}
	spec.scope = { topics, refusal: s.refusal, strict: s.strict, ...keep };
}

/** A strict scope the validator would refuse: no topic, or no answer for the rest. */
export function strictIncomplete(s: Scope): boolean {
	return s.strict && (!s.topics.some((t) => t.trim()) || !s.refusal.trim());
}

/* ---- knowledge & abilities ------------------------------------------ */

export const RAG_SEARCH = 'rag_search';
export const RAG_LIST = 'rag_list_collections';
const MCP_PREFIX = 'mcp__';
const connectorPrefix = (key: string) => `${MCP_PREFIX}${key}__`;

export interface Ability {
	/** `tool` / `connector` / `skill`: the grant kind; `rag_collection` for knowledge. */
	kind: 'rag_collection' | 'tool' | 'connector' | 'skill';
	ref: string;
	name: string;
	description: string | null;
	/** Tools a connector brings, by id. */
	tools: string[];
	on: boolean;
	/** The manager holds it and can therefore grant or revoke it. */
	holdable: boolean;
}

const tools = (spec: Spec): string[] => (Array.isArray(spec.main?.tools) ? spec.main.tools : []);
const skills = (spec: Spec): string[] => (Array.isArray(spec.main?.skills) ? spec.main.skills : []);

/**
 * Every card the abilities step shows: what the manager could grant, plus
 * whatever the agent already has that they could not (shown, not offered).
 * Knowledge is on when its collection is granted; the rest when the spec
 * uses it.
 */
export function abilities(spec: Spec, grants: Grant[], resources: AgentResources | null): Ability[] {
	const granted = (kind: string) => grants.filter((g) => g.kind === kind).map((g) => g.ref);
	const used = tools(spec);
	const out: Ability[] = [];

	const collections = resources?.rag_collections ?? [];
	for (const c of collections) {
		out.push({ kind: 'rag_collection', ref: String(c.id), name: c.name, description: null, tools: [], on: granted('rag_collection').includes(String(c.id)), holdable: true });
	}
	for (const ref of granted('rag_collection')) {
		if (!collections.some((c) => String(c.id) === ref)) {
			out.push({ kind: 'rag_collection', ref, name: ref, description: null, tools: [], on: true, holdable: false });
		}
	}

	const offered = (resources?.tools ?? []).filter((x) => x.id !== RAG_SEARCH && x.id !== RAG_LIST);
	for (const tool of offered) {
		out.push({ kind: 'tool', ref: tool.id, name: tool.name, description: tool.description, tools: [tool.id], on: used.includes(tool.id), holdable: true });
	}
	for (const id of used) {
		if (id.startsWith(MCP_PREFIX) || id === RAG_SEARCH || id === RAG_LIST || offered.some((x) => x.id === id)) continue;
		out.push({ kind: 'tool', ref: id, name: id, description: null, tools: [id], on: true, holdable: false });
	}

	const connectors = resources?.connectors ?? [];
	const connectorOn = (key: string) => used.some((id) => id.startsWith(connectorPrefix(key)));
	for (const c of connectors) {
		out.push({ kind: 'connector', ref: c.key, name: c.name, description: null, tools: c.tools, on: connectorOn(c.key), holdable: true });
	}
	const otherKeys = new Set(
		used
			.filter((id) => id.startsWith(MCP_PREFIX))
			.map((id) => id.slice(MCP_PREFIX.length).split('__')[0])
			.filter((key) => !connectors.some((c) => c.key === key))
	);
	for (const key of otherKeys) {
		out.push({ kind: 'connector', ref: key, name: key, description: null, tools: used.filter((id) => id.startsWith(connectorPrefix(key))), on: true, holdable: false });
	}

	const held = resources?.skills ?? [];
	for (const name of held) out.push({ kind: 'skill', ref: name, name, description: null, tools: [], on: skills(spec).includes(name), holdable: true });
	for (const name of skills(spec)) {
		if (!held.includes(name)) out.push({ kind: 'skill', ref: name, name, description: null, tools: [], on: true, holdable: false });
	}
	return out;
}

function dropTool(spec: Spec, id: string): void {
	spec.main.tools = tools(spec).filter((x) => x !== id);
	if (spec.main.tool_resources) delete spec.main.tool_resources[id];
}

function addTool(spec: Spec, id: string): void {
	if (!tools(spec).includes(id)) spec.main.tools = [...tools(spec), id];
}

/** Puts a tool, a connector's tools or a skill into the spec, or takes it out. */
export function setAbility(spec: Spec, ability: Pick<Ability, 'kind' | 'ref' | 'tools'>, on: boolean): void {
	spec.main ??= {};
	if (ability.kind === 'skill') {
		const rest = skills(spec).filter((s) => s !== ability.ref);
		spec.main.skills = on ? [...rest, ability.ref] : rest;
		return;
	}
	if (ability.kind === 'connector') {
		for (const id of tools(spec).filter((x) => x.startsWith(connectorPrefix(ability.ref)))) dropTool(spec, id);
		if (on) for (const id of ability.tools) addTool(spec, id);
		return;
	}
	if (ability.kind === 'tool') {
		if (on) addTool(spec, ability.ref);
		else dropTool(spec, ability.ref);
	}
}

/**
 * Wires knowledge search to the collections chosen (by name): one collection
 * is bound as a constant, so the model cannot search anything else; several
 * leave the choice to the model, which the grants already limit, and add the
 * listing tool when the manager may grant it.
 */
export function setKnowledge(spec: Spec, names: string[], canList: boolean): void {
	spec.main ??= {};
	if (!names.length) {
		dropTool(spec, RAG_SEARCH);
		dropTool(spec, RAG_LIST);
		return;
	}
	addTool(spec, RAG_SEARCH);
	const resources = (spec.main.tool_resources ??= {});
	const search = (resources[RAG_SEARCH] ??= {});
	if (names.length === 1) {
		search.bind = { ...(search.bind ?? {}), collection: { const: names[0] } };
		dropTool(spec, RAG_LIST);
	} else {
		if (search.bind) delete search.bind.collection;
		if (canList) addTool(spec, RAG_LIST);
	}
}

/** Whether the published version still relies on a grant, so revoking it would break the live agent. */
export function liveUses(live: Spec | null, kind: string, ref: string): boolean {
	if (!live) return false;
	const used = tools(live);
	switch (kind) {
		case 'pool':
			return (
				live.main?.pool === ref ||
				live.scope?.classifier_pool === ref ||
				live.router?.pool === ref ||
				live.publish?.voice?.speech_pool === ref ||
				live.publish?.voice?.transcription_pool === ref
			);
		case 'tool':
			return used.includes(ref);
		case 'connector':
			return used.some((id) => id.startsWith(connectorPrefix(ref))) || Object.values(live.verifiers ?? {}).some((v: any) => v?.connector === ref);
		case 'skill':
			return skills(live).includes(ref);
		case 'rag_collection':
			return used.includes(RAG_SEARCH);
		default:
			return false;
	}
}

/* ---- information to collect ------------------------------------------ */

export const SLOT_KINDS = ['text', 'long_text', 'email', 'phone', 'customer_number', 'order_number', 'date', 'number', 'whole_number', 'yes_no', 'choice'] as const;
export type SlotKind = (typeof SLOT_KINDS)[number];

/** Each friendly kind's slot type and validator (`docs/agents.md` §3 State). */
export const SLOT_SHAPES: Record<SlotKind, Spec> = {
	text: { type: 'string', max_length: 200 },
	long_text: { type: 'string', max_length: 2000 },
	email: { type: 'email' },
	phone: { type: 'string', pattern: '^\\+?[0-9][0-9 ()/-]{5,24}$' },
	customer_number: { type: 'string', pattern: '^[A-Za-z0-9][A-Za-z0-9-]{2,39}$' },
	order_number: { type: 'string', pattern: '^[A-Za-z0-9#][A-Za-z0-9#/-]{2,39}$' },
	date: { type: 'string', pattern: '^\\d{4}-\\d{2}-\\d{2}$' },
	number: { type: 'number' },
	whole_number: { type: 'integer' },
	yes_no: { type: 'boolean' },
	choice: { type: 'enum' }
};

/** Slots the identity and hand-off steps own; the details step does not list them. */
export const VERIFIED_SLOT = 'verified';
export const TOPIC_SLOT = 'topic';
export const REQUEST_SLOT = 'request';
const MANAGED_SLOTS = new Set([VERIFIED_SLOT, TOPIC_SLOT, REQUEST_SLOT]);

export interface SlotRow {
	key: string;
	label: string;
	kind: SlotKind | 'custom';
	values: string[];
	/** Not saved yet: its key follows the label until it is. */
	fresh?: boolean;
}


export function slotKind(def: Spec): SlotKind | 'custom' {
	const setBy = Array.isArray(def?.set_by) ? def.set_by : [];
	if (setBy.length !== 1 || setBy[0] !== 'llm') return 'custom';
	const rest: Spec = { ...def };
	delete rest.set_by;
	delete rest.description;
	delete rest.order;
	if (rest.type === 'enum') {
		delete rest.values;
		return Object.keys(rest).length === 1 ? 'choice' : 'custom';
	}
	return SLOT_KINDS.find((k) => k !== 'choice' && same(rest, SLOT_SHAPES[k])) ?? 'custom';
}

export function slotDef(kind: SlotKind, label: string, values: string[] = []): Spec {
	return {
		...clone(SLOT_SHAPES[kind]),
		...(kind === 'choice' ? { values: values.map((v) => v.trim()).filter(Boolean) } : {}),
		set_by: ['llm'],
		...(label.trim() ? { description: label.trim() } : {})
	};
}

/**
 * The details in the order the person gave them: by each slot's `order`
 * (a JSON object's key order does not survive a save — the server hands keys
 * back sorted), slots without one after, by name.
 */
export function readSlots(spec: Spec): SlotRow[] {
	const position = (def: Spec) => (Number.isInteger(def?.order) ? (def.order as number) : Number.MAX_SAFE_INTEGER);
	return Object.entries((spec.state ?? {}) as Record<string, Spec>)
		.filter(([key]) => !MANAGED_SLOTS.has(key))
		.sort(([a, da], [b, db]) => position(da) - position(db) || a.localeCompare(b))
		.map(([key, def]) => ({
			key,
			label: typeof def?.description === 'string' && def.description ? def.description : humanize(key),
			kind: slotKind(def),
			values: Array.isArray(def?.values) ? def.values.map(String) : []
		}));
}

/** The key a row is saved under; a fresh row's follows its label. */
export function slotKeys(rows: SlotRow[], reserved: Iterable<string>): string[] {
	const taken = new Set(reserved);
	for (const row of rows) if (!row.fresh) taken.add(row.key);
	return rows.map((row) => {
		if (!row.fresh) return row.key;
		const key = unique(identFrom(row.label, 'detail'), taken);
		taken.add(key);
		return key;
	});
}

export function writeSlots(spec: Spec, rows: SlotRow[]): void {
	const before = (spec.state ?? {}) as Record<string, Spec>;
	const managed = Object.entries(before).filter(([key]) => MANAGED_SLOTS.has(key));
	const keys = slotKeys(rows, managed.map(([k]) => k));
	const next: Record<string, Spec> = {};
	rows.forEach((row, i) => {
		next[keys[i]] = { ...(row.kind === 'custom' ? before[row.key] : slotDef(row.kind, row.label, row.values)), order: i };
	});
	for (const [key, def] of managed) next[key] = def;
	spec.state = next;
}

/** Slots the identity check reads, which the details step may not remove. */
export function slotsForIdentity(spec: Spec): string[] {
	const v = spec.verifiers?.[IDENTITY_VERIFIER];
	if (!v) return [];
	if (v.kind === 'mcp_code') return [String(v.email_slot ?? 'email')];
	if (v.kind === 'lookup') {
		return Object.values((v.inputs ?? {}) as Record<string, unknown>)
			.filter((s): s is string => typeof s === 'string' && s.startsWith('state.'))
			.map((s) => s.slice(6).split('.')[0]);
	}
	return [];
}

/* ---- identity check -------------------------------------------------- */

export const IDENTITY_VERIFIER = 'identity';
export const IDENTITY_METHODS = ['none', 'email_code', 'signed_in', 'customer_lookup'] as const;
export type IdentityMethod = (typeof IDENTITY_METHODS)[number];

export interface Identity {
	method: IdentityMethod;
	/** `email_code`: the connector that sends and checks the code. */
	connector: string;
	/** `customer_lookup`: the tool that confirms name and number. */
	tool: string;
	/** `signed_in`: what the website's token must carry. */
	issuer: string;
	audience: string;
	/** A new shared secret; blank keeps the stored one. */
	secret: string;
	secretSet: boolean;
	/** Verifiers the advanced editor set up besides this one. */
	others: number;
}

/** Labels for the slots the identity check creates when they are missing, in the manager's language. */
export interface IdentityLabels {
	email: string;
	name: string;
	customerNumber: string;
}

const METHOD_KIND: Record<Exclude<IdentityMethod, 'none'>, string> = {
	email_code: 'mcp_code',
	signed_in: 'host_jwt',
	customer_lookup: 'lookup'
};

export function readIdentity(spec: Spec): Identity {
	const v = spec.verifiers?.[IDENTITY_VERIFIER] ?? null;
	const method = (Object.keys(METHOD_KIND) as Exclude<IdentityMethod, 'none'>[]).find((m) => METHOD_KIND[m] === v?.kind) ?? 'none';
	return {
		method,
		connector: String(v?.connector ?? ''),
		tool: String(v?.tool ?? ''),
		issuer: String(v?.issuer ?? ''),
		audience: String(v?.audience ?? ''),
		secret: '',
		secretSet: !!v?.secret_sealed,
		others: Object.keys(spec.verifiers ?? {}).filter((id) => id !== IDENTITY_VERIFIER).length
	};
}

/** Who writes the confirmed identity: what a gate's `provenance` names. */
export function identityWriter(spec: Spec): string | null {
	const kind = spec.verifiers?.[IDENTITY_VERIFIER]?.kind;
	if (!kind) return null;
	return kind === 'host_jwt' ? 'host' : `verifier:${IDENTITY_VERIFIER}`;
}

function ensureSlot(spec: Spec, key: string, def: Spec): void {
	spec.state ??= {};
	spec.state[key] ??= def;
}

function rewriteProvenance(cond: any, from: string, to: string): any {
	if (Array.isArray(cond)) return cond.map((c) => rewriteProvenance(c, from, to));
	if (!cond || typeof cond !== 'object') return cond;
	const out: Spec = {};
	for (const [k, v] of Object.entries(cond)) out[k] = rewriteProvenance(v, from, to);
	if (out.slot === VERIFIED_SLOT && out.provenance === from) out.provenance = to;
	return out;
}

export function writeIdentity(spec: Spec, id: Identity, labels: IdentityLabels): void {
	const before = spec.verifiers?.[IDENTITY_VERIFIER] ?? {};
	const fromWriter = identityWriter(spec);
	if (spec.verifiers) delete spec.verifiers[IDENTITY_VERIFIER];
	if (spec.verifiers && !Object.keys(spec.verifiers).length) delete spec.verifiers;

	if (id.method === 'none') {
		if (spec.state) delete spec.state[VERIFIED_SLOT];
		return;
	}
	const kind = METHOD_KIND[id.method];
	const kept: Spec = before.kind === kind ? clone(before) : {};
	let verifier: Spec;
	if (id.method === 'email_code') {
		ensureSlot(spec, 'email', slotDef('email', labels.email));
		verifier = { ...kept, kind, email_slot: 'email', writes: kept.writes ?? { verified: 'result' } };
		if (id.connector) verifier.connector = id.connector;
		else delete verifier.connector;
	} else if (id.method === 'customer_lookup') {
		ensureSlot(spec, 'name', slotDef('text', labels.name));
		ensureSlot(spec, 'customer_number', slotDef('customer_number', labels.customerNumber));
		verifier = {
			...kept,
			kind,
			inputs: kept.inputs ?? { name: 'state.name', number: 'state.customer_number' },
			writes: kept.writes ?? { verified: 'result' },
			assurance: kept.assurance ?? 'low'
		};
		if (id.tool) verifier.tool = id.tool;
		else delete verifier.tool;
	} else {
		verifier = {
			...kept,
			kind,
			algorithm: kept.algorithm ?? 'HS256',
			claims: kept.claims ?? { verified: { customer_id: 'sub' } }
		};
		for (const key of ['issuer', 'audience'] as const) {
			if (id[key].trim()) verifier[key] = id[key].trim();
			else delete verifier[key];
		}
		if (id.secret) {
			verifier.secret = id.secret;
			delete verifier.secret_sealed;
		}
	}
	spec.verifiers = { ...(spec.verifiers ?? {}), [IDENTITY_VERIFIER]: verifier };

	const writer = identityWriter(spec) as string;
	spec.state ??= {};
	spec.state[VERIFIED_SLOT] = { ...(spec.state[VERIFIED_SLOT] ?? {}), type: 'subject', set_by: [writer] };
	if (fromWriter && fromWriter !== writer && spec.routes) {
		for (const route of Object.values(spec.routes as Record<string, Spec>)) {
			if (route?.when) route.when = rewriteProvenance(route.when, fromWriter, writer);
		}
	}
}

/** What the identity method still needs before the agent can be published. */
export function identityMissing(spec: Spec): 'connector' | 'tool' | 'token' | null {
	const v = spec.verifiers?.[IDENTITY_VERIFIER];
	if (!v) return null;
	if (v.kind === 'mcp_code' && !v.connector) return 'connector';
	if (v.kind === 'lookup' && !v.tool) return 'tool';
	if (v.kind === 'host_jwt' && (!v.issuer || !v.audience || !(v.secret || v.secret_sealed || v.public_key || v.jwks_url))) return 'token';
	return null;
}

/** A random shared secret for the website's tokens: 48 characters, comfortably over the 32 the server asks for. */
export function newSecret(bytes: Uint8Array = crypto.getRandomValues(new Uint8Array(48))): string {
	const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
	return Array.from(bytes, (n) => alphabet[n % alphabet.length]).join('');
}

/* ---- hand-offs ------------------------------------------------------- */

export const FALLBACK_ROUTE = 'fallback';
export const HANDOFF_TASK = 'Request about {topic}: {request}';
const TOPIC_DESCRIPTION = 'What the request is about.';
const REQUEST_DESCRIPTION = 'A short summary of what the visitor needs.';

export type Target = { kind: 'agent'; id: string } | { kind: 'human' };

export interface Rule {
	/** The route it was read from; `null` for a new rule. */
	route: string | null;
	topic: string;
	identity: boolean;
	target: Target;
	/** What the route passes the specialist, derived from its live spec ([`deriveBind`]). */
	bind: Record<string, unknown>;
}

export interface Handoffs {
	rules: Rule[];
	fallback: boolean;
	/** Routes the advanced editor set up, kept as they are. */
	custom: string[];
}

const REQUEST_SET = { slot: REQUEST_SLOT, set: true };
/** JSON equality regardless of key order: the server hands the spec back with its keys sorted. */
function canonical(value: unknown): unknown {
	if (Array.isArray(value)) return value.map(canonical);
	if (value && typeof value === 'object') {
		return Object.fromEntries(Object.keys(value).sort().map((k) => [k, canonical((value as Record<string, unknown>)[k])]));
	}
	return value;
}
const same = (a: unknown, b: unknown) => JSON.stringify(canonical(a)) === JSON.stringify(canonical(b));

function targetOf(route: Spec): Target | null {
	const kinds = ['agent', 'human', 'a2a', 'loop'].filter((k) => k in route);
	if (kinds.length !== 1) return null;
	if (typeof route.agent === 'string') return { kind: 'agent', id: route.agent };
	if (route.human && typeof route.human === 'object') return { kind: 'human' };
	return null;
}

/** The rule a route is, when it has exactly the shape [`writeHandoffs`] gives one. */
export function ruleOf(name: string, route: Spec): Rule | null {
	const target = targetOf(route ?? {});
	const all = route?.when?.all;
	if (!target || !Array.isArray(all) || Object.keys(route.when).length !== 1) return null;
	const [topic, request, verified, ...rest] = all;
	if (rest.length || !same(request, REQUEST_SET)) return null;
	if (!topic || topic.slot !== TOPIC_SLOT || typeof topic.eq !== 'string' || Object.keys(topic).length !== 2) return null;
	if (verified && !(verified.slot === VERIFIED_SLOT && typeof verified.provenance === 'string' && Object.keys(verified).length === 2)) return null;
	if (target.kind === 'agent' && route.task !== HANDOFF_TASK) return null;
	return { route: name, topic: topic.eq, identity: !!verified, target, bind: route.bind ? clone(route.bind) : {} };
}

const isFallback = (name: string, route: Spec) =>
	name === FALLBACK_ROUTE && same(route?.when, REQUEST_SET) && targetOf(route)?.kind === 'human';

function routeOrder(spec: Spec): string[] {
	const names = Object.keys(spec.routes ?? {});
	const order: string[] = Array.isArray(spec.router?.order) ? spec.router.order.filter((n: string) => names.includes(n)) : [];
	return [...order, ...names.filter((n) => !order.includes(n))];
}

export function readHandoffs(spec: Spec): Handoffs {
	const out: Handoffs = { rules: [], fallback: false, custom: [] };
	for (const name of routeOrder(spec)) {
		const route = spec.routes[name];
		const rule = ruleOf(name, route);
		if (rule) out.rules.push(rule);
		else if (isFallback(name, route)) out.fallback = true;
		else out.custom.push(name);
	}
	return out;
}

export function writeHandoffs(spec: Spec, h: Handoffs): void {
	const before = (spec.routes ?? {}) as Record<string, Spec>;
	const writer = identityWriter(spec);
	const rules = h.rules.filter((r) => r.topic.trim());
	const taken = new Set([...h.custom, FALLBACK_ROUTE]);
	const routes: Record<string, Spec> = {};
	const ruleNames: string[] = [];

	for (const rule of rules) {
		const topic = rule.topic.trim();
		const name = rule.route && !taken.has(rule.route) ? rule.route : unique(identFrom(topic, 'handoff'), taken);
		taken.add(name);
		ruleNames.push(name);
		const when: Spec[] = [{ slot: TOPIC_SLOT, eq: topic }, { ...REQUEST_SET }];
		if (rule.identity && writer) when.push({ slot: VERIFIED_SLOT, provenance: writer });
		const prev = rule.route ? before[rule.route] : undefined;
		const route: Spec = { description: topic, when: { all: when } };
		if (rule.target.kind === 'agent') {
			route.agent = rule.target.id;
			route.task = HANDOFF_TASK;
			if (Object.keys(rule.bind).length) route.bind = clone(rule.bind);
		} else {
			route.human = prev?.human ? clone(prev.human) : {};
		}
		routes[name] = route;
	}
	for (const name of h.custom) if (before[name]) routes[name] = before[name];
	if (h.fallback) {
		const prev = before[FALLBACK_ROUTE];
		routes[FALLBACK_ROUTE] = { when: clone(REQUEST_SET), human: prev?.human && isFallback(FALLBACK_ROUTE, prev) ? clone(prev.human) : {} };
	}
	spec.routes = routes;

	spec.state ??= {};
	const topics = [...new Set(rules.map((r) => r.topic.trim()))];
	if (topics.length) spec.state[TOPIC_SLOT] = { type: 'enum', values: topics, set_by: ['llm'], description: TOPIC_DESCRIPTION };
	else delete spec.state[TOPIC_SLOT];
	const customUsesRequest = h.custom.some((n) => JSON.stringify(routes[n] ?? {}).includes(`"${REQUEST_SLOT}"`) || String(routes[n]?.task ?? '').includes(`{${REQUEST_SLOT}`));
	if (rules.length || h.fallback) spec.state[REQUEST_SLOT] ??= { ...slotDef('long_text', REQUEST_DESCRIPTION) };
	else if (!customUsesRequest) delete spec.state[REQUEST_SLOT];

	const order = [...ruleNames, ...h.custom.filter((n) => routes[n]), ...(h.fallback ? [FALLBACK_ROUTE] : [])];
	if (order.length) spec.router = { ...(spec.router ?? { kind: 'rules' }), order };
	else if (spec.router) {
		delete spec.router.order;
		if (same(spec.router, { kind: 'rules' })) delete spec.router;
	}
}

/**
 * The route values a specialist needs, filled from this agent's trusted
 * state: a slot of that name the model cannot write, else that field of the
 * confirmed identity. What neither can supply is `missing`.
 */
export function deriveBind(specialist: Spec | null, spec: Spec): { bind: Record<string, string>; missing: string[] } {
	const names = new Set<string>();
	for (const resource of Object.values((specialist?.main?.tool_resources ?? {}) as Record<string, Spec>)) {
		for (const source of Object.values((resource?.bind ?? {}) as Record<string, unknown>)) {
			if (typeof source === 'string' && source.startsWith('route.')) names.add(source.slice(6));
		}
	}
	const bind: Record<string, string> = {};
	const missing: string[] = [];
	for (const name of names) {
		const slot = spec.state?.[name];
		if (slot && Array.isArray(slot.set_by) && !slot.set_by.includes('llm')) bind[name] = `state.${name}`;
		else if (spec.state?.[VERIFIED_SLOT]) bind[name] = `state.${VERIFIED_SLOT}.${name}`;
		else missing.push(name);
	}
	return { bind, missing };
}

/** Hand-off topics whose rule asks for a confirmed identity: they need an identity check to exist. */
export function topicsNeedingIdentity(spec: Spec): string[] {
	return readHandoffs(spec).rules.filter((r) => r.identity).map((r) => r.topic);
}

/* ---- website -------------------------------------------------------- */

/** `https://www.example.com/path` → `https://www.example.com`; `null` for what is not a web address. */
export function originOf(text: string): string | null {
	const trimmed = text.trim();
	if (!trimmed) return null;
	try {
		const url = new URL(/^[a-z]+:\/\//i.test(trimmed) ? trimmed : `https://${trimmed}`);
		return url.protocol === 'https:' || url.protocol === 'http:' ? url.origin : null;
	} catch {
		return null;
	}
}

export function readSite(spec: Spec): string[] {
	return Array.isArray(spec.publish?.origins) ? spec.publish.origins.map(String) : [];
}

export function writeSite(spec: Spec, origins: string[]): void {
	const valid = [...new Set(origins.map(originOf).filter((o): o is string => !!o))];
	if (valid.length) {
		spec.publish ??= {};
		spec.publish.origins = valid;
		return;
	}
	if (spec.publish) {
		delete spec.publish.origins;
		if (!Object.keys(spec.publish).length) delete spec.publish;
	}
}

/** The widget's colour (`profile.color`): `#rrggbb`, or `''` for the widget's own. */
export function readColor(spec: Spec): string {
	const color = String(spec.profile?.color ?? '').trim().toLowerCase();
	return /^#[0-9a-f]{6}$/.test(color) ? color : '';
}

export function writeColor(spec: Spec, color: string): void {
	const value = color.trim().toLowerCase();
	if (/^#[0-9a-f]{6}$/.test(value)) {
		spec.profile ??= {};
		spec.profile.color = value;
		return;
	}
	if (spec.profile) {
		delete spec.profile.color;
		if (!Object.keys(spec.profile).length) delete spec.profile;
	}
}

/** `publish.voice`: what visitors may say and hear, and the pool each runs on. */
export interface Voice {
	input: boolean;
	output: boolean;
	/** `''` for the speech pool's default voice for the visitor's language. */
	voice: string;
	transcriptionPool: string;
	speechPool: string;
}

export function readVoice(spec: Spec): Voice {
	const v = spec.publish?.voice ?? {};
	return {
		input: v.input === true,
		output: v.output === true,
		voice: typeof v.voice === 'string' ? v.voice : '',
		transcriptionPool: typeof v.transcription_pool === 'string' ? v.transcription_pool : '',
		speechPool: typeof v.speech_pool === 'string' ? v.speech_pool : ''
	};
}

export function writeVoice(spec: Spec, v: Voice): void {
	const out: Record<string, unknown> = {};
	if (v.input) out.input = true;
	if (v.output) out.output = true;
	if (v.voice.trim()) out.voice = v.voice.trim();
	if (v.transcriptionPool) out.transcription_pool = v.transcriptionPool;
	if (v.speechPool) out.speech_pool = v.speechPool;
	if (Object.keys(out).length) {
		spec.publish ??= {};
		spec.publish.voice = out;
		return;
	}
	if (spec.publish) {
		delete spec.publish.voice;
		if (!Object.keys(spec.publish).length) delete spec.publish;
	}
}

/** The pools a voice direction that is on still needs before the agent can be published. */
export function voiceMissing(v: Voice): Array<'transcription' | 'speech'> {
	const missing: Array<'transcription' | 'speech'> = [];
	if (v.input && !v.transcriptionPool) missing.push('transcription');
	if (v.output && !v.speechPool) missing.push('speech');
	return missing;
}

/* ---- templates ------------------------------------------------------ */

export const TEMPLATES = ['faq', 'support', 'leads', 'internal', 'blank'] as const;
export type TemplateKey = (typeof TEMPLATES)[number];

function resolveKeys(value: unknown, tr: (key: string) => string): unknown {
	if (typeof value === 'string') return value.startsWith('@') ? tr(value.slice(1)) : value;
	if (Array.isArray(value)) return value.map((v) => resolveKeys(v, tr));
	if (value && typeof value === 'object') {
		return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, resolveKeys(v, tr)]));
	}
	return value;
}

/** A template's starter spec, its texts in the manager's language (`tr` is `t`). */
export function templateSpec(key: TemplateKey, tr: (key: string) => string): Spec {
	return resolveKeys(clone((templates as Record<string, unknown>)[key]), tr) as Spec;
}

/**
 * The spec after choosing a template: the template's, with the agent's own
 * name and the model already chosen carried over, so picking a template
 * never undoes the step before.
 */
export function applyTemplate(spec: Spec, key: TemplateKey, tr: (key: string) => string): Spec {
	const next = templateSpec(key, tr);
	if (spec.profile?.display) next.profile = { ...(next.profile ?? {}), display: spec.profile.display };
	if (spec.main?.pool) next.main = { ...(next.main ?? {}), pool: spec.main.pool };
	return next;
}

/** A spec with nothing a person set up yet, where a template replaces nothing. */
export function isBlank(spec: Spec): boolean {
	const s = clone(spec);
	if (s.profile) delete s.profile.display;
	if (s.main) delete s.main.pool;
	const empty = (v: unknown): boolean =>
		v === undefined || v === null || v === '' || (Array.isArray(v) && v.length === 0) ||
		(typeof v === 'object' && !Array.isArray(v) && Object.values(v as object).every(empty));
	return empty(s);
}

/* ---- checklist and status ------------------------------------------- */

export interface Todo {
	/** The step that fixes it; `null` for what only the advanced editor reaches. */
	step: StepKey | null;
	/** A catalog key, or `null` when `message` is the server's own text. */
	key: string | null;
	args?: Record<string, string | number>;
	message?: string;
	/** Publishing is held back while it is open. */
	blocking: boolean;
}

/** The step whose part of the spec a validator path points into. */
export function stepForPath(path: string): StepKey | null {
	const head = (prefixes: string[]) => prefixes.some((p) => path === p || path.startsWith(`${p}.`) || path.startsWith(`${p}[`));
	if (head(['profile.color', 'publish.voice'])) return 'site';
	if (head(['main.pool', 'main.instructions', 'profile'])) return 'basics';
	if (head(['scope'])) return 'scope';
	if (head(['main.tools', 'main.skills', 'main.tool_resources'])) return 'abilities';
	if (head(['verifiers', `state.${VERIFIED_SLOT}`])) return 'identity';
	if (head(['routes', 'router', `state.${TOPIC_SLOT}`, `state.${REQUEST_SLOT}`])) return 'routes';
	if (head(['state'])) return 'slots';
	if (head(['publish.origins'])) return 'site';
	return null;
}

export function checklist(spec: Spec, publishIssues: SpecIssue[] = []): Todo[] {
	const todos: Todo[] = [];
	const basics = readBasics(spec);
	if (!basics.pool) todos.push({ step: 'basics', key: 'agents-setup-todo-model', blocking: true });
	if (!basics.task.trim()) todos.push({ step: 'basics', key: 'agents-setup-todo-task', blocking: true });
	if (strictIncomplete(readScope(spec))) todos.push({ step: 'scope', key: 'agents-setup-strict-needs', blocking: true });
	const identity = identityMissing(spec);
	if (identity) todos.push({ step: 'identity', key: `agents-setup-todo-identity-${identity}`, blocking: true });
	for (const rule of readHandoffs(spec).rules) {
		if (rule.target.kind === 'agent' && !rule.target.id) {
			todos.push({ step: 'routes', key: 'agents-setup-rule-no-target', args: { topic: rule.topic }, blocking: true });
		}
	}
	if (!readSite(spec).length) todos.push({ step: 'site', key: 'agents-setup-todo-site', blocking: false });
	const seen = new Set<string>();
	for (const issue of publishIssues) {
		const message = issue.path ? `${issue.path}: ${issue.message}` : issue.message;
		if (seen.has(message)) continue;
		seen.add(message);
		todos.push({ step: stepForPath(issue.path), key: null, message, blocking: true });
	}
	return todos;
}

export type SectionStatus = 'open' | 'done' | 'optional';

export function sectionStatus(step: StepKey, spec: Spec, todos: Todo[]): SectionStatus {
	if (todos.some((t) => t.step === step)) return 'open';
	const configured: Partial<Record<StepKey, boolean>> = {
		basics: !!readBasics(spec).task.trim() && !!spec.main?.pool,
		scope: readScope(spec).topics.length > 0,
		abilities: tools(spec).length > 0 || skills(spec).length > 0,
		slots: readSlots(spec).length > 0,
		identity: readIdentity(spec).method !== 'none',
		routes: (() => {
			const h = readHandoffs(spec);
			return h.rules.length > 0 || h.fallback || h.custom.length > 0;
		})(),
		site: readSite(spec).length > 0
	};
	return configured[step] ? 'done' : 'optional';
}

/* ---- one-line summaries --------------------------------------------- */

export interface SummaryContext {
	tr: (key: string, args?: Record<string, string | number>) => string;
	grants: Grant[];
	resources: AgentResources | null;
	agents: { id: string; name: string; display: string }[];
}

const clip = (text: string, max = 140) => (text.length > max ? `${text.slice(0, max - 1).trimEnd()}…` : text);

/** The overview's plain-language line for one section. */
export function summary(step: StepKey, spec: Spec, ctx: SummaryContext): string {
	const { tr } = ctx;
	switch (step) {
		case 'basics': {
			const b = readBasics(spec);
			const tier = tierOf(b.pool, ctx.resources?.tiers);
			const model = tier ? tr(`agents-setup-model-${tier}`) : b.pool ? humanize(b.pool.replace(/-/g, '_')) : tr('agents-setup-sum-model-none');
			const task = b.task.trim() ? clip(b.task.trim().split('\n')[0]) : tr('agents-setup-sum-no-task');
			return tr('agents-setup-sum-basics', { task, model });
		}
		case 'scope': {
			const s = readScope(spec);
			if (!s.topics.length) return tr('agents-setup-sum-scope-none');
			return tr(s.strict ? 'agents-setup-sum-scope-strict' : 'agents-setup-sum-scope-soft', { topics: s.topics.join(', ') });
		}
		case 'abilities': {
			const on = abilities(spec, ctx.grants, ctx.resources).filter((c) => c.on).map((c) => c.name);
			return on.length ? on.join(' · ') : tr('agents-setup-sum-abilities-none');
		}
		case 'slots': {
			const rows = readSlots(spec);
			return rows.length ? rows.map((r) => r.label).join(', ') : tr('agents-setup-sum-slots-none');
		}
		case 'identity':
			return tr(`agents-setup-identity-${readIdentity(spec).method}`);
		case 'routes': {
			const h = readHandoffs(spec);
			const person = tr('agents-setup-rule-person');
			const target = (r: Rule) => {
				if (r.target.kind === 'human') return person;
				const id = r.target.id;
				const agent = ctx.agents.find((a) => a.id === id);
				return agent ? agent.display || agent.name : tr('agents-pick');
			};
			const parts = h.rules.map((r) => `${r.topic} → ${target(r)}`);
			if (h.fallback) parts.push(tr('agents-setup-sum-routes-other', { target: person }));
			if (h.custom.length) parts.push(tr('agents-setup-routes-custom', { count: h.custom.length }));
			return parts.length ? parts.join(' · ') : tr('agents-setup-sum-routes-none');
		}
		case 'site': {
			const origins = readSite(spec);
			return origins.length ? origins.join(', ') : tr('agents-setup-sum-site-none');
		}
		default:
			return '';
	}
}

/* ---- the prompt assistant's proposal (#117) ----------------------- */

const SUGGESTED_METHOD: Record<string, IdentityMethod> = {
	none: 'none',
	email_code: 'email_code',
	website_login: 'signed_in',
	lookup: 'customer_lookup'
};

/** The identity card a proposed method stands for; `null` for one this step does not know. */
export function suggestedMethod(method: string): IdentityMethod | null {
	return SUGGESTED_METHOD[method] ?? null;
}

/** Proposed details as rows of the details step: their own key, a friendly kind where one fits, and none twice. */
export function suggestedSlotRows(slots: { name: string; label: string; def: Spec }[], existing: SlotRow[]): SlotRow[] {
	const taken = new Set(existing.map((r) => r.key));
	return slots
		.filter((s) => !taken.has(s.name) && !MANAGED_SLOTS.has(s.name))
		.map((s): SlotRow => {
			const kind = slotKind(s.def);
			const values = Array.isArray(s.def?.values) ? s.def.values.map(String) : [];
			return { key: s.name, label: s.label, kind: kind === 'custom' ? (s.def?.type === 'string' ? 'text' : 'custom') : kind, values, fresh: false };
		})
		.filter((r) => r.kind !== 'custom');
}

/** Proposed hand-offs as the sentences of the hand-off step; the gate, task and slots follow from them. */
export function suggestedRules(handoffs: { topic: string; target: string }[], existing: Rule[]): Rule[] {
	const topics = new Set(existing.map((r) => r.topic.trim().toLowerCase()));
	return handoffs
		.filter((h) => {
			const topic = h.topic.trim().toLowerCase();
			if (!topic || topics.has(topic)) return false;
			topics.add(topic);
			return true;
		})
		.map((h) => ({ route: null, topic: h.topic.trim(), identity: false, target: h.target === 'human' ? { kind: 'human' as const } : { kind: 'agent' as const, id: h.target }, bind: {} }));
}
