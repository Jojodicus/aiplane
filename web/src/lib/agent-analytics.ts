/**
 * The pure half of the agent Analytics tab: the shape of
 * `GET /api/v0/agents/{id}/analytics`, the query it is asked with, and the
 * geometry of the per-day bar chart. No framework imports, so `node --test`
 * covers it.
 */

export interface DayBucket {
	day: string;
	conversations: number;
	turns: number;
	tokens: number;
	cost: number;
	refusals: number;
}

export interface AgentAnalytics {
	from: string;
	to: string;
	version: number | null;
	currency: string;
	conversations: number;
	turns: number;
	sub_agents: {
		dispatched: number;
		finished: number;
		incomplete: number;
		incomplete_by_reason: Record<string, number>;
	};
	gate_refusals: {
		total: number;
		by_route: Record<string, number>;
		by_missing_slot: { route: string; slot: string; count: number }[];
	};
	routes_chosen: Record<string, number>;
	output_blocks: { total: number; by_action: Record<string, number> };
	limit_refusals: { total: number; by_kind: Record<string, number> };
	human_handoffs: number;
	usage: {
		requests: number;
		prompt_tokens: number;
		completion_tokens: number;
		tokens: number;
		cost: number;
	};
	daily: DayBucket[];
}

export const RANGE_DAYS = [7, 30, 90] as const;

const isoDay = (d: Date) => d.toISOString().slice(0, 10);

/** The query for the last `days` UTC days up to and including today. */
export function analyticsQuery(days: number, version: number | null, now: Date): string {
	const from = new Date(now.getTime() - (days - 1) * 86_400_000);
	const params = new URLSearchParams({ from: isoDay(from), to: isoDay(now) });
	if (version !== null) params.set('version', String(version));
	return params.toString();
}

/** A map as rows, biggest count first and name order among equals. */
export function countRows(counts: Record<string, number>): [string, number][] {
	return Object.entries(counts).sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]));
}

export type Metric = 'conversations' | 'turns' | 'tokens' | 'cost' | 'refusals';
export const METRICS: Metric[] = ['conversations', 'turns', 'tokens', 'cost', 'refusals'];

export interface Bar {
	day: string;
	value: number;
	x: number;
	y: number;
	width: number;
	height: number;
}

/**
 * Bars for `metric` in a `width` x `height` box. Heights scale to the largest
 * value; a non-zero value is never thinner than one unit, so a quiet day next
 * to a busy one stays visible. All zeros give flat zero-height bars.
 */
export function bars(daily: DayBucket[], metric: Metric, width: number, height: number): Bar[] {
	if (daily.length === 0) return [];
	const slot = width / daily.length;
	const max = Math.max(...daily.map((d) => d[metric]));
	return daily.map((d, i) => {
		const value = d[metric];
		const h = max > 0 && value > 0 ? Math.max(1, (value / max) * height) : 0;
		return {
			day: d.day,
			value,
			x: i * slot + slot * 0.1,
			y: height - h,
			width: slot * 0.8,
			height: h
		};
	});
}
