<script lang="ts">
	import StatusPill from '#lib/components/ui/StatusPill.svelte';
	import CapabilityBrowser from '#lib/components/capabilities/CapabilityBrowser.svelte';
	import { grantable, type GrantableItem, type Spec } from '#lib/agents.js';
	import { LOCKED_GROUP, RAG_LIST, RAG_SEARCH, abilities, requireKnowledgeSearch, setAbility, setKnowledge, type Ability } from '#lib/agent-setup.js';
	import { useWorkspace } from '#lib/agent-workspace.svelte.js';
	import { rankCapabilities } from '#lib/capability-picker.js';
	import { t } from '#lib/i18n.svelte.js';
	import { toolCategoryLabel } from '#lib/tools.js';
	import SuggestionBox from './SuggestionBox.svelte';

	/**
	 * Knowledge and abilities in the list the chat picker uses: the
	 * resources the manager may grant, each with its own title and
	 * description, switched on or off. Switching one on stages the grant it
	 * needs and puts it into the spec; switching it off takes it out and
	 * stages revoking the grant unless the published version still relies on
	 * it. The grants are made when the draft is saved (the manager's own
	 * rights cap them, and a refusal is shown then), never on the click.
	 */
	let { spec = $bindable() }: { spec: Spec } = $props();
	const ws = useWorkspace();
	const suggested = $derived(ws.suggestion?.steps.abilities ?? []);
	const suggestedKnowledge = $derived(ws.suggestion?.steps.knowledge ?? []);
	const missingKnowledge = $derived(ws.suggestion?.steps.missing_knowledge ?? []);

	const cards = $derived(abilities(spec, ws.grants, ws.resources));
	const knowledge = $derived(cards.filter((c) => c.kind === 'rag_collection'));
	const cardOf = $derived(new Map(cards.map((c) => [c.item, c])));
	const offers = (id: string) => grantable(ws.resources, 'tool').some((x) => x.grant.refs.includes(id));

	let error = $state<string | null>(null);
	let kept = $state<Ability[]>([]);

	function isSuggested(c: Ability): boolean {
		if (c.kind === 'rag_collection') return suggestedKnowledge.some((k) => k.id === c.ref);
		return c.kind === 'tool' && suggested.some((s) => c.refs.includes(s.id));
	}
	const rank = (item: GrantableItem) => {
		const c = cardOf.get(item);
		return c?.on ? 0 : c && isSuggested(c) ? 1 : 2;
	};
	const groupLabel = (group: string) => (group === LOCKED_GROUP ? t('agents-setup-locked') : toolCategoryLabel(group));

	function collectionNames(): string[] {
		return ws.grants
			.filter((g) => g.kind === 'rag_collection')
			.map((g) => grantable(ws.resources, 'rag_collection').find((c) => c.grant.refs.includes(g.ref))?.title ?? g.ref);
	}

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
				kept = [...kept, c];
			}
			const names = collectionNames();
			const canList = names.length > 1 && offers(RAG_LIST);
			if (canList) ws.stageGrant('tool', RAG_LIST);
			setKnowledge(spec, names, canList);
			return;
		}
		if (on) {
			for (const ref of c.refs) ws.stageGrant(c.kind, ref);
			setAbility(spec, c, true);
		} else {
			setAbility(spec, c, false);
			if (c.refs.map((ref) => ws.stageRevoke(c.kind, ref)).some(Boolean)) kept = [...kept, c];
		}
	}
	const wasKept = (c: Ability) => !c.on && kept.some((k) => k.kind === c.kind && k.ref === c.ref);
</script>

<div class="flex flex-col gap-5">
	<p class="m-0 text-base-content/70">{t('agents-setup-abilities-lead')}</p>
	{#if error}<div class="alert alert-error text-sm" role="alert"><span>{error}</span></div>{/if}
	<SuggestionBox part={['abilities', 'knowledge', 'missing_knowledge']} onapply={suggested.length || suggestedKnowledge.length ? applySuggested : null}>
		<ul class="m-0 flex list-none flex-col gap-1 p-0">
			{#each suggestedKnowledge as k (k.id)}<li><span class="font-semibold">{k.name}</span>{#if k.why} — {k.why}{/if}</li>{/each}
			{#each suggested as s (s.id)}<li><span class="font-semibold">{s.name}</span>{#if s.why} — {s.why}{/if}</li>{/each}
		</ul>
		{#each missingKnowledge as topic (topic)}
			<p class="m-0 mt-1.5 text-warning">{t('agents-setup-suggest-knowledge-missing', { topic })}</p>
		{/each}
	</SuggestionBox>

	{#if cards.length}
		<h3 class="sr-only" id="agent-abilities">{t('agents-setup-step-abilities')}</h3>
		<div class="flex h-[36rem] flex-col overflow-hidden rounded-box border border-base-300">
			<CapabilityBrowser items={cards.map((c) => c.item)} labelledby="agent-abilities" order={(rows) => rankCapabilities(rows, rank)} {groupLabel} searchPlaceholder={t('agents-setup-abilities-search')}>
				{#snippet control(item)}
					{@const c = cardOf.get(item)}
					{#if c}
						<input class="toggle toggle-primary shrink-0" type="checkbox" checked={c.on} disabled={!c.holdable} aria-label={t('tools-toggle-aria', { name: item.title })} onchange={() => toggle(c)} />
					{/if}
				{/snippet}
				{#snippet detail(item)}
					{@const c = cardOf.get(item)}
					{#if c && !c.holdable}
						<span class="mt-1 flex flex-wrap items-center gap-2">
							<StatusPill size="sm">{t('agents-setup-locked')}</StatusPill>
							<span class="text-xs text-base-content/60">{t('agents-setup-locked-hint')}</span>
						</span>
					{:else if c && wasKept(c)}
						<p class="m-0 mt-1 text-xs text-base-content/60">{t('agents-setup-kept-live')}</p>
					{:else if c && !c.on && isSuggested(c)}
						<span class="badge badge-sm badge-outline mt-1 border-dashed border-primary/60 text-primary">✦ {t('ui-ai-suggestion')}</span>
					{/if}
				{/snippet}
			</CapabilityBrowser>
		</div>
	{:else}
		<div class="alert alert-info text-sm"><span>{t('agents-setup-nothing-available')}</span></div>
	{/if}

	<p class="m-0 text-sm text-base-content/60">{t('agents-setup-abilities-more')}</p>
</div>
