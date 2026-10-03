<script lang="ts">
	import { untrack } from 'svelte';
	import { agentsApi, type AgentError } from '$lib/agents';
	import {
		KIND_GROUPS,
		activityQuery,
		conversationsOf,
		exportQuery,
		groupByTurn,
		summarize,
		type ActivityEvent,
		type ActivityFilter,
		type KindGroup,
		type Verification
	} from '$lib/agent-activity';
	import { dt, t } from '$lib/i18n.svelte';

	/**
	 * The agent's activity log, from `GET …/activity`: everything its runs
	 * did — model exchanges, tool calls, state writes, routing, pauses — and
	 * every change to the agent, hash-chained per conversation. One
	 * conversation reads as a timeline by turn; the whole log newest first.
	 */
	let { agentId }: { agentId: string } = $props();

	const GROUPS = Object.keys(KIND_GROUPS) as KindGroup[];

	let filter = $state<ActivityFilter>({ conversation: '', group: 'all', from: '', to: '' });
	let events = $state<ActivityEvent[]>([]);
	let next = $state<number | null>(null);
	let known = $state<string[]>([]);
	let loading = $state(false);
	let error = $state<string | null>(null);
	let verification = $state<Verification | null>(null);
	let verifying = $state(false);
	let requested = 0;

	async function load(more = false) {
		const mine = ++requested;
		loading = true;
		try {
			const page = await agentsApi.activity(agentId, activityQuery(filter, more ? next : null));
			if (mine !== requested) return;
			events = more ? [...events, ...page.events] : page.events;
			next = page.next_cursor;
			for (const c of conversationsOf(page.events)) if (!known.includes(c)) known = [...known, c];
			error = null;
		} catch (err) {
			if (mine === requested) error = (err as AgentError).message;
		} finally {
			if (mine === requested) loading = false;
		}
	}

	$effect(() => {
		void filter.conversation;
		void filter.group;
		void filter.from;
		void filter.to;
		untrack(() => void load());
	});

	async function verify() {
		verifying = true;
		try {
			verification = await agentsApi.verifyActivity(agentId);
			error = null;
		} catch (err) {
			error = (err as AgentError).message;
		} finally {
			verifying = false;
		}
	}

	const groups = $derived(filter.conversation ? groupByTurn(events) : [{ turn: null, events }]);
	const exportHref = $derived(`/api/v0/agents/${agentId}/activity/export?${exportQuery(filter)}`);
	const pretty = (v: unknown) => JSON.stringify(v, null, 2);
	const line = (e: ActivityEvent) => {
		const s = summarize(e);
		return s ? t(s.key, s.args) : '';
	};
</script>

<div class="space-y-4">
	<p class="text-sm text-base-content/70">{t('agents-act-intro')}</p>

	<div class="flex flex-wrap items-end gap-3">
		<label class="fieldset">
			<span class="fieldset-legend text-xs">{t('agents-act-conversation')}</span>
			<select class="select select-sm max-w-xs" bind:value={filter.conversation}>
				<option value="">{t('agents-act-conversation-all')}</option>
				{#each known as c (c)}<option value={c}>{c}</option>{/each}
			</select>
		</label>
		<label class="fieldset">
			<span class="fieldset-legend text-xs">{t('agents-act-kind')}</span>
			<select class="select select-sm" bind:value={filter.group}>
				{#each GROUPS as g (g)}<option value={g}>{t(`agents-act-group-${g}`)}</option>{/each}
			</select>
		</label>
		<label class="fieldset">
			<span class="fieldset-legend text-xs">{t('agents-act-from')}</span>
			<input type="date" class="input input-sm" bind:value={filter.from} />
		</label>
		<label class="fieldset">
			<span class="fieldset-legend text-xs">{t('agents-act-to')}</span>
			<input type="date" class="input input-sm" bind:value={filter.to} />
		</label>
		<div class="flex gap-2">
			<a class="btn btn-sm" href={exportHref} download>{t('agents-act-export')}</a>
			<button type="button" class="btn btn-sm" disabled={verifying} onclick={verify}>
				{#if verifying}<span class="loading loading-spinner loading-xs" aria-hidden="true"></span>{/if}
				{t('agents-act-verify')}
			</button>
		</div>
		{#if loading}<span class="loading loading-spinner loading-sm" aria-hidden="true"></span>{/if}
	</div>

	{#if verification}
		{#if verification.ok}
			<div class="alert alert-success text-sm" role="status">
				<span>
					{t('agents-act-verified', { events: verification.events, chains: verification.chains })}
					{#if verification.unanchored}{t('agents-act-unanchored', { count: verification.unanchored })}{/if}
				</span>
			</div>
			{#if verification.head}
				<p class="text-xs text-base-content/70">
					{t('agents-act-head')}
					<code class="block break-all">{verification.head.chain_key} #{verification.head.seq} {verification.head.hash ?? ''}</code>
				</p>
			{/if}
		{:else if verification.broken}
			<div class="alert alert-error text-sm" role="alert">
				<span>{t('agents-act-broken', { chain: verification.broken.chain_key, seq: verification.broken.seq, reason: verification.broken.reason })}</span>
			</div>
		{/if}
	{/if}
	{#if error}<div class="alert alert-error text-sm" role="alert"><span>{error}</span></div>{/if}

	{#if !events.length && !loading}
		<p class="text-sm text-base-content/60">{t('agents-act-none')}</p>
	{/if}

	{#each groups as group, i (i)}
		<section class="space-y-1">
			{#if filter.conversation}
				<h4 class="text-xs font-semibold text-base-content/60">
					{group.turn ? t('agents-act-turn', { turn: group.turn }) : t('agents-act-agent-chain')}
				</h4>
			{/if}
			<ul class="space-y-1">
				{#each group.events as event (event.id)}
					<li class="collapse collapse-arrow rounded-box border border-base-300 bg-base-100">
						<input type="checkbox" aria-label={event.kind} />
						<div class="collapse-title flex min-w-0 flex-wrap items-center gap-2 py-2 text-sm">
							<code class="badge badge-sm badge-outline">{event.kind}</code>
							<span class="text-xs text-base-content/60">{dt(event.ts)}</span>
							{#if event.round !== null}<span class="badge badge-ghost badge-xs">{t('agents-act-round', { round: event.round })}</span>{/if}
							{#if event.duration_ms !== null}<span class="text-xs text-base-content/60">{t('agents-act-took', { ms: event.duration_ms })}</span>{/if}
							{#if event.principal_id !== agentId && event.agent_id === agentId}<span class="badge badge-info badge-xs">{t('agents-act-sub-agent')}</span>{/if}
							<span class="min-w-0 truncate">{line(event)}</span>
						</div>
						<div class="collapse-content space-y-2 text-xs">
							{#if !filter.conversation && event.conversation_id}
								<button type="button" class="btn btn-ghost btn-xs" onclick={() => (filter.conversation = event.conversation_id ?? '')}>{t('agents-act-open-conversation')}</button>
							{/if}
							<pre class="max-h-96 overflow-auto rounded-box bg-base-200 p-2 whitespace-pre-wrap break-words">{pretty(event.detail)}</pre>
							<p class="font-mono break-all text-base-content/50">#{event.seq ?? '–'} · {event.hash ?? ''}</p>
						</div>
					</li>
				{/each}
			</ul>
		</section>
	{/each}

	{#if next !== null}
		<button type="button" class="btn btn-sm" disabled={loading} onclick={() => load(true)}>{t('agents-act-more')}</button>
	{/if}
</div>
