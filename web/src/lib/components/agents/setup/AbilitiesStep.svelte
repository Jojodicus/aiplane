<script lang="ts">
	import ChoiceCard from '$lib/components/ui/ChoiceCard.svelte';
	import StatusPill from '$lib/components/ui/StatusPill.svelte';
	import type { Spec } from '$lib/agents';
	import { RAG_LIST, RAG_SEARCH, abilities, requireKnowledgeSearch, setAbility, setKnowledge, type Ability } from '$lib/agent-setup';
	import { abilityTitle, filterAbilities, orderAbilities, plainText, visibleAbilities } from '$lib/ability-list';
	import { useWorkspace } from '$lib/agent-workspace.svelte';
	import { t } from '$lib/i18n.svelte';
	import SuggestionBox from './SuggestionBox.svelte';

	/**
	 * Knowledge and abilities as cards. Switching one on stages the grant it
	 * needs and puts it into the spec; switching it off takes it out and stages
	 * revoking the grant unless the published version still relies on it. The
	 * grants are made when the draft is saved (the manager's own rights cap
	 * them, and a refusal is shown then), never on the click.
	 */
	let { spec = $bindable() }: { spec: Spec } = $props();
	const ws = useWorkspace();
	const suggested = $derived(ws.suggestion?.steps.abilities ?? []);
	const suggestedIds = $derived(suggested.map((s) => s.id));

	const cards = $derived(abilities(spec, ws.grants, ws.resources));
	const knowledge = $derived(cards.filter((c) => c.kind === 'rag_collection'));
	const others = $derived(cards.filter((c) => c.kind !== 'rag_collection'));
	let query = $state('');
	let showAll = $state(false);
	const ordered = $derived(orderAbilities(others, suggestedIds));
	const matching = $derived(filterAbilities(ordered, query));
	const shown = $derived(visibleAbilities(matching, suggestedIds, showAll || query.trim() !== ''));
	const hidden = $derived(matching.length - shown.length);
	const offers = (id: string) => !!ws.resources?.tools.some((x) => x.id === id);

	let error = $state<string | null>(null);
	let kept = $state<string[]>([]);

	const cardId = (c: Ability) => `${c.kind}:${c.ref}`;

	function description(c: Ability): string | null {
		if (c.kind === 'rag_collection') return t('agents-setup-knowledge-desc', { name: c.name });
		if (c.kind === 'connector') return t('agents-setup-connector-desc', { name: c.name, count: c.tools.length });
		if (c.kind === 'skill') return t('agents-setup-skill-desc', { name: c.name });
		return c.description ? firstSentence(plainText(c.description)) : null;
	}
	/** A tool's description is written for the model and can run to a paragraph; the card shows its first sentence. */
	function firstSentence(text: string): string {
		const end = text.search(/[.!?](\s|$)/);
		const sentence = end > 0 ? text.slice(0, end + 1) : text;
		return sentence.length > 140 ? `${sentence.slice(0, 139).trimEnd()}…` : sentence;
	}
	const title = abilityTitle;

	function collectionNames(): string[] {
		return ws.grants
			.filter((g) => g.kind === 'rag_collection')
			.map((g) => ws.resources?.rag_collections.find((c) => String(c.id) === g.ref)?.name ?? g.ref);
	}

	const suggestedKnowledge = $derived(ws.suggestion?.steps.knowledge ?? []);
	const missingKnowledge = $derived(ws.suggestion?.steps.missing_knowledge ?? []);

	/**
	 * Every proposed tool and knowledge base, staged for granting and put
	 * into the spec like a switched-on card: knowledge as its card's
	 * `toggle` does it, the collection and the search granted, the search
	 * bound to what is on. Knowledge the agent could not search fails the
	 * apply, so the suggestion stays with the reason in it.
	 */
	function applySuggested() {
		for (const s of suggested) {
			ws.stageGrant('tool', s.id);
			setAbility(spec, { kind: 'tool', ref: s.id, tools: [s.id] }, true);
		}
		const off = suggestedKnowledge.filter((k) => !knowledge.some((c) => c.ref === k.id && c.on));
		requireKnowledgeSearch(off.map((k) => k.id), offers(RAG_SEARCH), t);
		if (!off.length) return;
		for (const k of off) ws.stageGrant('rag_collection', k.id);
		ws.stageGrant('tool', RAG_SEARCH);
		const names = collectionNames();
		const canList = names.length > 1 && offers(RAG_LIST);
		if (canList) ws.stageGrant('tool', RAG_LIST);
		setKnowledge(spec, names, canList);
	}

	/** Grants are staged, not made: the save that follows Next / Apply carries them out, Cancel drops them. */
	function toggle(c: Ability) {
		if (!c.holdable) return;
		error = null;
		const on = !c.on;
		if (c.kind === 'rag_collection') {
			if (on) {
				if (!offers(RAG_SEARCH)) {
					error = t('agents-setup-grant-failed', { reason: t('agents-setup-knowledge-no-search') });
					return;
				}
				ws.stageGrant('rag_collection', c.ref);
				ws.stageGrant('tool', RAG_SEARCH);
			} else if (ws.stageRevoke('rag_collection', c.ref)) {
				kept = [...kept, cardId(c)];
			}
			const names = collectionNames();
			const canList = names.length > 1 && offers(RAG_LIST);
			if (canList) ws.stageGrant('tool', RAG_LIST);
			setKnowledge(spec, names, canList);
			return;
		}
		if (on) {
			ws.stageGrant(c.kind, c.ref);
			setAbility(spec, c, true);
		} else {
			setAbility(spec, c, false);
			if (ws.stageRevoke(c.kind, c.ref)) kept = [...kept, cardId(c)];
		}
	}
</script>

{#snippet card(c: Ability)}
	<ChoiceCard multiple title={title(c)} description={description(c)} selected={c.on} disabled={!c.holdable} onselect={() => toggle(c)}>
		{#snippet tag()}
			{#if !c.holdable}
				<span class="flex flex-col gap-1">
					<StatusPill size="sm">{t('agents-setup-locked')}</StatusPill>
					<span class="text-xs text-base-content/60">{t('agents-setup-locked-hint')}</span>
				</span>
			{:else if kept.includes(cardId(c)) && !c.on}
				<span class="text-xs text-base-content/60">{t('agents-setup-kept-live')}</span>
			{/if}
		{/snippet}
	</ChoiceCard>
{/snippet}

<div class="flex flex-col gap-5">
	<p class="m-0 text-base-content/70">{t('agents-setup-abilities-lead')}</p>
	{#if error}<div class="alert alert-error text-sm" role="alert"><span>{error}</span></div>{/if}
	<SuggestionBox part={['abilities', 'knowledge', 'missing_knowledge']} onapply={applySuggested}>
		<ul class="m-0 flex list-none flex-col gap-1 p-0">
			{#each suggestedKnowledge as k (k.id)}
				<li><span class="font-semibold">{t('agents-setup-knowledge-desc', { name: k.name })}</span>{#if k.why} — {k.why}{/if}</li>
			{/each}
			{#each suggested as s (s.id)}<li><span class="font-semibold">{s.name}</span>{#if s.why} — {s.why}{/if}</li>{/each}
		</ul>
		{#each missingKnowledge as topic (topic)}
			<p class="m-0 mt-1.5 text-warning">{t('agents-setup-suggest-knowledge-missing', { topic })}</p>
		{/each}
	</SuggestionBox>

	{#if !cards.length}
		<div class="alert alert-info text-sm"><span>{t('agents-setup-nothing-available')}</span></div>
	{/if}

	{#if knowledge.length}
		<section class="flex flex-col gap-2">
			<h3 class="m-0 text-sm font-semibold uppercase tracking-wider text-base-content/60">{t('agents-setup-knowledge')}</h3>
			<div class="grid gap-2.5 sm:grid-cols-2" role="group" aria-label={t('agents-setup-knowledge')}>
				{#each knowledge as c (cardId(c))}{@render card(c)}{/each}
			</div>
		</section>
	{/if}

	{#if others.length}
		<section class="flex flex-col gap-2">
			<h3 class="m-0 text-sm font-semibold uppercase tracking-wider text-base-content/60">{t('agents-setup-abilities')}</h3>
			<input class="input w-full max-w-sm" type="search" bind:value={query} placeholder={t('agents-setup-abilities-search')} aria-label={t('agents-setup-abilities-search')} />
			<div class="grid gap-2.5 sm:grid-cols-2" role="group" aria-label={t('agents-setup-abilities')}>
				{#each shown as c (cardId(c))}{@render card(c)}{/each}
			</div>
			{#if query.trim() && !matching.length}
				<p class="m-0 text-sm text-base-content/60">{t('agents-setup-abilities-none-found')}</p>
			{/if}
			{#if !shown.length && !query.trim()}
				<p class="m-0 text-sm text-base-content/60">{t('agents-setup-abilities-none-chosen')}</p>
			{/if}
			{#if hidden > 0}
				<button class="btn btn-sm self-start" type="button" onclick={() => (showAll = true)}>{t('agents-setup-abilities-show-all', { count: matching.length })}</button>
			{/if}
		</section>
	{/if}

	<p class="m-0 text-sm text-base-content/60">{t('agents-setup-abilities-more')}</p>
</div>
