/**
 * The agent builder's canvas: the fixed topology of a spec (main agent →
 * gates → routes → targets, `docs/agents.md`) as nodes and edges with a
 * deterministic layered layout, plus the mapping from the validator's
 * `path`s and the test chat's debug view onto those nodes. Pure: the canvas
 * edits the same spec the form builder does, through the helpers in
 * `agents.ts`.
 */
import type { Spec, SpecIssue, TestDebug } from './agents.ts';

export type NodeKind = 'main' | 'gate' | 'route' | 'target';

export interface CanvasNode {
	id: string;
	kind: NodeKind;
	/** The route a gate, route or target node belongs to. */
	route?: string;
	/** For a target: `agent`, `human` or whatever other kind key the route carries (`a2a`, `loop`, …). */
	targetKind?: string;
	x: number;
	y: number;
	w: number;
	h: number;
}

export interface CanvasEdge {
	id: string;
	from: string;
	to: string;
	route?: string;
}

export interface CanvasGraph {
	nodes: CanvasNode[];
	edges: CanvasEdge[];
	width: number;
	height: number;
}

export const SIZES = {
	main: { w: 260, h: 176 },
	gate: { w: 200, h: 84 },
	route: { w: 200, h: 84 },
	target: { w: 220, h: 84 }
} as const;
export const GAP_X = 72;
export const GAP_Y = 24;
export const PAD = 16;

/** Keys of a route that are not its target kind. */
const ROUTE_KEYS = new Set(['when', 'description', 'task', 'bind']);

export function targetKindOf(route: Spec | undefined): string {
	if (!route) return 'agent';
	if ('human' in route) return 'human';
	if ('agent' in route) return 'agent';
	return Object.keys(route).find((k) => !ROUTE_KEYS.has(k)) ?? 'agent';
}

export const nodeId = (kind: NodeKind, route?: string): string => (kind === 'main' ? 'main' : `${kind}:${route}`);

/**
 * One row per route in spec order, one column per layer. The main agent is
 * centred against the rows; nothing depends on anything but the spec, so the
 * same spec always lays out the same way.
 */
export function layoutCanvas(spec: Spec): CanvasGraph {
	const names = Object.keys(spec.routes ?? {});
	const rowH = Math.max(SIZES.gate.h, SIZES.target.h);
	const rows = Math.max(names.length, 1);
	const bodyH = rows * rowH + (rows - 1) * GAP_Y;
	const height = Math.max(bodyH, SIZES.main.h) + 2 * PAD;
	const colX = (i: number) =>
		PAD + [SIZES.main.w, SIZES.gate.w, SIZES.route.w].slice(0, i).reduce((sum, w) => sum + w + GAP_X, 0);
	const rowY = (h: number, row: number) =>
		PAD + (height - 2 * PAD - bodyH) / 2 + row * (rowH + GAP_Y) + (rowH - h) / 2;

	const nodes: CanvasNode[] = [
		{ id: 'main', kind: 'main', ...SIZES.main, x: colX(0), y: (height - SIZES.main.h) / 2 }
	];
	const edges: CanvasEdge[] = [];
	names.forEach((name, row) => {
		let prev = 'main';
		(['gate', 'route', 'target'] as const).forEach((kind, i) => {
			const node: CanvasNode = {
				id: nodeId(kind, name),
				kind,
				route: name,
				...SIZES[kind],
				x: colX(i + 1),
				y: rowY(SIZES[kind].h, row)
			};
			if (kind === 'target') node.targetKind = targetKindOf(spec.routes[name]);
			nodes.push(node);
			edges.push({ id: `${prev}>${node.id}`, from: prev, to: node.id, route: name });
			prev = node.id;
		});
	});
	return { nodes, edges, width: colX(3) + SIZES.target.w + PAD, height };
}

/** The SVG path of an edge: a horizontal S-curve from the right side of `from` to the left side of `to`. */
export function edgePath(from: CanvasNode, to: CanvasNode): string {
	const x1 = from.x + from.w;
	const y1 = from.y + from.h / 2;
	const x2 = to.x;
	const y2 = to.y + to.h / 2;
	const mid = (x1 + x2) / 2;
	return `M${x1} ${y1} C${mid} ${y1} ${mid} ${y2} ${x2} ${y2}`;
}

/* ---- validation ------------------------------------------------------ */

const MAIN_PATHS = ['main', 'state', 'router', 'verifiers'];

/** The node id a spec `path` belongs to, or null when no node shows that part of the spec (profile, finish, publish, …). */
export function nodeForPath(spec: Spec, path: string): string | null {
	if (MAIN_PATHS.some((p) => path === p || path.startsWith(`${p}.`) || path.startsWith(`${p}[`))) return 'main';
	const m = /^routes\.([^.[]+)(?:[.[](.*))?$/.exec(path);
	if (!m) return null;
	const [, name, rest] = m;
	if (rest === undefined) return nodeId('route', name);
	const key = rest.split(/[.[]/)[0];
	if (key === 'when') return nodeId('gate', name);
	if (key === 'description') return nodeId('route', name);
	if (key === 'task' || key === 'bind') return nodeId('target', name);
	return key === targetKindOf(spec.routes?.[name]) ? nodeId('target', name) : nodeId('route', name);
}

/** The issues each node carries, by node id. */
export function issuesByNode(spec: Spec, issues: SpecIssue[]): Map<string, SpecIssue[]> {
	const out = new Map<string, SpecIssue[]>();
	for (const issue of issues) {
		const id = nodeForPath(spec, issue.path);
		if (id) out.set(id, [...(out.get(id) ?? []), issue]);
	}
	return out;
}

/* ---- summaries ------------------------------------------------------- */

export interface MainSummary {
	/** `main.model`; `''` for the gateway default. */
	model: string;
	tools: number;
	skills: number;
	slots: string[];
	/** Verifiers attached to the main agent, by id and `kind`, whatever kinds exist. */
	verifiers: { id: string; kind: string }[];
}

export function summarizeMain(spec: Spec): MainSummary {
	return {
		model: spec.main?.model ?? '',
		tools: (spec.main?.tools ?? []).length,
		skills: (spec.main?.skills ?? []).length,
		slots: Object.keys(spec.state ?? {}),
		verifiers: Object.entries((spec.verifiers ?? {}) as Record<string, Spec>).map(([id, v]) => ({ id, kind: String(v?.kind ?? '') }))
	};
}

const show = (v: unknown): string => (typeof v === 'string' ? v : JSON.stringify(v));

/** A one-line reading of a gate: leaves as `slot = value`, `slot ∈ [..]` or `slot <set>`, combined with `&`, `|` and `!`. */
export function describeCond(cond: Spec | undefined, setLabel: string): string {
	if (!cond || typeof cond !== 'object') return '';
	if (Array.isArray(cond.all)) return cond.all.map((c: Spec) => wrap(c, setLabel)).join(' & ');
	if (Array.isArray(cond.any)) return cond.any.map((c: Spec) => wrap(c, setLabel)).join(' | ');
	if ('not' in cond) return `!${wrap(cond.not, setLabel)}`;
	const parts: string[] = [];
	if ('eq' in cond) parts.push(`${cond.slot} = ${show(cond.eq)}`);
	else if (Array.isArray(cond.in)) parts.push(`${cond.slot} ∈ [${cond.in.map(show).join(', ')}]`);
	else parts.push(`${cond.slot} ${setLabel}`);
	if (cond.provenance) parts.push(`@${cond.provenance}`);
	if (cond.max_age) parts.push(`≤ ${cond.max_age}`);
	return parts.join(' ');
}

function wrap(cond: Spec, setLabel: string): string {
	const text = describeCond(cond, setLabel);
	return Array.isArray(cond?.all) || Array.isArray(cond?.any) ? `(${text})` : text;
}

/* ---- the last test turn --------------------------------------------- */

export interface TestPath {
	/** Per route: is its gate open on the state after the turn. */
	open: Record<string, boolean>;
	/** The route the router picked this turn, if any. */
	picked: string | null;
	/** Routes whose sub-agent ran this turn, with how it ended. */
	dispatched: Record<string, string | undefined>;
}

export function testPath(debug: TestDebug | null | undefined): TestPath | null {
	if (!debug) return null;
	const picks = (debug.routing ?? []).map((r) => r.picked).filter((p): p is string => !!p);
	const dispatched: Record<string, string | undefined> = {};
	for (const s of debug.sub_agents ?? []) if (s.route) dispatched[s.route] = s.outcome?.status;
	return {
		open: Object.fromEntries((debug.routes ?? []).map((r) => [r.route, r.open])),
		picked: picks.length ? picks[picks.length - 1] : null,
		dispatched
	};
}

/** True when an edge lies on the path the last test turn took. */
export function edgeOnPath(edge: CanvasEdge, path: TestPath | null): boolean {
	return !!path && !!edge.route && (edge.route === path.picked || edge.route in path.dispatched);
}
