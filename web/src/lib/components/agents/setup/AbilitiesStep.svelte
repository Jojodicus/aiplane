<script lang="ts">
	import ChoiceCard from '$lib/components/ui/ChoiceCard.svelte';
	import StatusPill from '$lib/components/ui/StatusPill.svelte';
	import type { AgentError, Spec } from '$lib/agents';
	import { RAG_LIST, RAG_SEARCH, abilities, humanize, liveUses, setAbility, setKnowledge, type Ability } from '$lib/agent-setup';
	import { useWorkspace } from '$lib/agent-workspace.svelte';
	import { t } from '$lib/i18n.svelte';

	/**
	 * Knowledge and abilities as cards. Switching one on grants what it needs
	 * to the agent's principal right away (the manager's own rights cap it, and
	 * the server's refusal is shown) and puts it into the spec; switching it
	 * off takes it out and revokes the grant unless the published version
	 * still relies on it.
	 */
	let { spec = $bindable() }: { spec: Spec } = $props();
	const ws = useWorkspace();

	const cards = $derived(abilities(spec, ws.detail?.grants ?? [], ws.resources));
	const knowledge = $derived(cards.filter((c) => c.kind === 'rag_collection'));
	const others = $derived(cards.filter((c) => c.kind !== 'rag_collection'));
	const offers = (id: string) => !!ws.resources?.tools.some((x) => x.id === id);

	let busy = $state<string | null>(null);
	let error = $state<string | null>(null);
	let kept = $state<string[]>([]);

	const cardId = (c: Ability) => `${c.kind}:${c.ref}`;

	function description(c: Ability): string | null {
		if (c.kind === 'rag_collection') return t('agents-setup-knowledge-desc', { name: c.name });
		if (c.kind === 'connector') return t('agents-setup-connector-desc', { name: c.name, count: c.tools.length });
		if (c.kind === 'skill') return t('agents-setup-skill-desc', { name: c.name });
		return c.description ? firstSentence(c.description) : null;
	}
	/** A tool's description is written for the model and can run to a paragraph; the card shows its first sentence. */
	function firstSentence(text: string): string {
		const end = text.search(/[.!?](\s|$)/);
		const sentence = end > 0 ? text.slice(0, end + 1) : text;
		return sentence.length > 140 ? `${sentence.slice(0, 139).trimEnd()}…` : sentence;
	}
	const title = (c: Ability) => (c.kind === 'tool' && c.name === c.ref ? humanize(c.name) : c.name);

	function collectionNames(): string[] {
		return (ws.detail?.grants ?? [])
			.filter((g) => g.kind === 'rag_collection')
			.map((g) => ws.resources?.rag_collections.find((c) => String(c.id) === g.ref)?.name ?? g.ref);
	}

	async function toggle(c: Ability) {
		if (!c.holdable || busy) return;
		busy = cardId(c);
		error = null;
		const on = !c.on;
		try {
			if (c.kind === 'rag_collection') {
				if (on) {
					if (!offers(RAG_SEARCH)) throw { message: t('agents-setup-knowledge-no-search') };
					await ws.ensureGrant('rag_collection', c.ref);
					await ws.ensureGrant('tool', RAG_SEARCH);
				} else if (!(await ws.releaseGrant('rag_collection', c.ref))) {
					kept = [...kept, cardId(c)];
				}
				const names = collectionNames();
				const canList = names.length > 1 && offers(RAG_LIST);
				if (canList) await ws.ensureGrant('tool', RAG_LIST);
				setKnowledge(spec, names, canList);
				return;
			}
			if (on) {
				await ws.ensureGrant(c.kind, c.ref);
				setAbility(spec, c, true);
			} else {
				setAbility(spec, c, false);
				if (liveUses(ws.detail?.live_spec ?? null, c.kind, c.ref)) kept = [...kept, cardId(c)];
				else await ws.releaseGrant(c.kind, c.ref);
			}
		} catch (err) {
			error = t('agents-setup-grant-failed', { reason: (err as AgentError).message });
		} finally {
			busy = null;
		}
	}
</script>

{#snippet card(c: Ability)}
	<ChoiceCard multiple title={title(c)} description={description(c)} selected={c.on} disabled={!c.holdable || busy !== null} onselect={() => void toggle(c)}>
		{#snippet tag()}
			{#if !c.holdable}
				<span class="flex flex-col gap-1">
					<StatusPill size="sm">{t('agents-setup-locked')}</StatusPill>
					<span class="text-xs text-base-content/60">{t('agents-setup-locked-hint')}</span>
				</span>
			{:else if busy === cardId(c)}
				<span class="loading loading-spinner loading-xs"></span>
			{:else if kept.includes(cardId(c)) && !c.on}
				<span class="text-xs text-base-content/60">{t('agents-setup-kept-live')}</span>
			{/if}
		{/snippet}
	</ChoiceCard>
{/snippet}

<div class="flex flex-col gap-5">
	<p class="m-0 text-base-content/70">{t('agents-setup-abilities-lead')}</p>
	{#if error}<div class="alert alert-error text-sm" role="alert"><span>{error}</span></div>{/if}

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
			<div class="grid gap-2.5 sm:grid-cols-2" role="group" aria-label={t('agents-setup-abilities')}>
				{#each others as c (cardId(c))}{@render card(c)}{/each}
			</div>
		</section>
	{/if}

	<p class="m-0 text-sm text-base-content/60">{t('agents-setup-abilities-more')}</p>
</div>
