import { humanize, type Ability } from './agent-setup.ts';

export const abilityTitle = (c: Ability) => (c.kind === 'tool' && c.item.title === c.ref ? humanize(c.ref) : c.item.title);

export const plainText = (text: string) => text.replaceAll('`', '');

/** Switched on first, then what the assistant proposes, then the rest by title. */
export function orderAbilities(list: Ability[], suggested: string[]): Ability[] {
	const rank = (c: Ability) => (c.on ? 0 : suggested.includes(c.ref) ? 1 : 2);
	return [...list].sort((a, b) => rank(a) - rank(b) || abilityTitle(a).localeCompare(abilityTitle(b)));
}

export function filterAbilities(list: Ability[], query: string): Ability[] {
	const q = query.trim().toLowerCase();
	if (!q) return list;
	return list.filter((c) => `${abilityTitle(c)} ${c.item.description}`.toLowerCase().includes(q));
}

/**
 * An ordered list cut to the cards switched on or proposed, unless everything is asked for:
 * the first few of an alphabet are no recommendation, and a visitor-facing agent needs few abilities.
 */
export function visibleAbilities(ordered: Ability[], suggested: string[], showAll: boolean): Ability[] {
	return showAll ? ordered : ordered.filter((c) => c.on || suggested.includes(c.ref));
}
