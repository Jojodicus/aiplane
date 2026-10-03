import { humanize, type Ability } from './agent-setup.ts';

export const DEFAULT_EXTRA = 6;

export const abilityTitle = (c: Ability) => (c.kind === 'tool' && c.name === c.ref ? humanize(c.name) : c.name);

export const plainText = (text: string) => text.replaceAll('`', '');

/** Switched on first, then what the assistant proposes, then the rest by title. */
export function orderAbilities(list: Ability[], suggested: string[]): Ability[] {
	const rank = (c: Ability) => (c.on ? 0 : suggested.includes(c.ref) ? 1 : 2);
	return [...list].sort((a, b) => rank(a) - rank(b) || abilityTitle(a).localeCompare(abilityTitle(b)));
}

export function filterAbilities(list: Ability[], query: string): Ability[] {
	const q = query.trim().toLowerCase();
	if (!q) return list;
	return list.filter((c) => `${abilityTitle(c)} ${c.description ?? ''}`.toLowerCase().includes(q));
}

/** An ordered list cut to the cards that matter plus a few more, unless everything is asked for. */
export function visibleAbilities(ordered: Ability[], suggested: string[], showAll: boolean): Ability[] {
	if (showAll) return ordered;
	const core = ordered.filter((c) => c.on || suggested.includes(c.ref));
	const rest = ordered.filter((c) => !c.on && !suggested.includes(c.ref));
	return [...core, ...rest.slice(0, DEFAULT_EXTRA)];
}
