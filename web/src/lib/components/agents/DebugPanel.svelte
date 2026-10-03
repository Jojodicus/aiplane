<script lang="ts">
	import { gateHint, type Spec, type TestDebug } from '$lib/agents';
	import { slotLabel } from '$lib/agent-setup';
	import { t } from '$lib/i18n.svelte';

	/**
	 * What a manager sees of a test turn that a visitor never does: every slot
	 * with its value and who wrote it, each route's gate and what keeps it
	 * closed, the topic guard's verdict, the router's decision, sub-agent calls
	 * and the grant decision on each tool call. All of it is read back from the stored state and the
	 * audit trail, not from the model's own account. Slots go by the labels
	 * `spec` gives them, with the id in small print.
	 */
	let { debug, spec = {} }: { debug: TestDebug; spec?: Spec } = $props();

	const label = (slot: string) => slotLabel(spec, slot, t);

	const show = (v: unknown) => (typeof v === 'string' ? v : JSON.stringify(v));
	const badge = (status: string) => (status === 'set' ? 'badge-success' : status === 'invalid' ? 'badge-error' : 'badge-ghost');
	const verdict = {
		in_scope: { key: 'agents-debug-scope-in-scope', badge: 'badge-success' },
		out_of_scope: { key: 'agents-debug-scope-out-of-scope', badge: 'badge-warning' },
		failed: { key: 'agents-debug-scope-failed', badge: 'badge-error' }
	} as const;
</script>

<div class="space-y-4 text-sm">
	<section>
		<h4 class="mb-1 font-semibold">{t('agents-debug-slots')}</h4>
		{#if debug.slots.length}
			<div class="overflow-x-auto">
				<table class="table table-xs">
					<tbody>
						{#each debug.slots as slot (slot.slot)}
							<tr>
								<td>
									{label(slot.slot)}
									{#if label(slot.slot) !== slot.slot}<span class="block font-mono text-xs text-base-content/50">{slot.slot}</span>{/if}
								</td>
								<td><span class="badge badge-sm {badge(slot.status)}">{t(`agents-slot-status-${slot.status}`)}</span></td>
								<td class="max-w-48 break-words font-mono text-xs">
									{#if slot.status === 'missing'}<span class="text-base-content/40">—</span>{:else}{show(slot.value)}{/if}
									{#if slot.reason}<span class="block text-error">{slot.reason}</span>{/if}
								</td>
								<td class="font-mono text-xs text-base-content/60">{slot.provenance ?? ''}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{:else}
			<p class="text-base-content/60">{t('agents-debug-no-slots')}</p>
		{/if}
	</section>

	<section>
		<h4 class="mb-1 font-semibold">{t('agents-debug-routes')}</h4>
		{#each debug.routes as route (route.route)}
			<div class="mb-2">
				<div class="flex items-center gap-2">
					<span>{route.description || route.route}</span>
					{#if route.description}<span class="font-mono text-xs text-base-content/50">{route.route}</span>{/if}
					<span class="badge badge-sm {route.open ? 'badge-success' : 'badge-warning'}">{route.open ? t('agents-gate-open') : t('agents-gate-closed')}</span>
				</div>
				{#if route.missing.length}
					<ul class="ml-4 list-disc text-xs text-base-content/70">
						{#each route.missing as unmet (unmet.path + unmet.message)}<li>{gateHint(unmet, label(unmet.slot ?? ''), t)}</li>{/each}
					</ul>
				{/if}
			</div>
		{:else}
			<p class="text-base-content/60">{t('agents-debug-no-routes')}</p>
		{/each}
	</section>

	{#if debug.scope}
		<section>
			<h4 class="mb-1 font-semibold">{t('agents-debug-scope')}</h4>
			<span class="badge badge-sm {verdict[debug.scope.verdict].badge}">{t(verdict[debug.scope.verdict].key)}</span>
			<p class="mt-1 text-xs text-base-content/60">{t('agents-debug-scope-topics', { topics: debug.scope.topics.join(', ') })}</p>
			{#if debug.scope.error}<p class="text-xs text-error">{debug.scope.error}</p>{/if}
		</section>
	{/if}

	<section>
		<h4 class="mb-1 font-semibold">{t('agents-debug-routing')}</h4>
		{#each debug.routing as decision, i (i)}
			<p>
				{#if decision.picked}{t('agents-debug-picked', { route: decision.picked })}{:else}{t('agents-debug-none-picked')}{/if}
				{#if decision.reason}<span class="text-base-content/60"> — {decision.reason}</span>{/if}
			</p>
		{:else}
			<p class="text-base-content/60">{t('agents-debug-no-routing')}</p>
		{/each}
	</section>

	<section>
		<h4 class="mb-1 font-semibold">{t('agents-debug-subagents')}</h4>
		{#each debug.sub_agents as call, i (i)}
			<div class="mb-2">
				<div class="flex flex-wrap items-center gap-2">
					<span class="font-mono">{call.sub_agent ?? call.remote_agent ?? call.card_url ?? call.sub_agent_id}</span>
					{#if call.route}<span class="badge badge-outline badge-sm">{call.route}</span>{/if}
					{#if call.loop}
						<span class="badge badge-ghost badge-sm">{t(call.loop.role === 'critic' ? 'agents-debug-loop-critic' : 'agents-debug-loop-worker', { iteration: call.loop.iteration })}</span>
					{/if}
					{#if call.outcome}
						<span class="badge badge-sm {call.outcome.status === 'finished' ? 'badge-success' : 'badge-warning'}">{call.outcome.status}</span>
					{:else}
						<span class="badge badge-ghost badge-sm">{t('agents-debug-running')}</span>
					{/if}
				</div>
				{#if call.outcome?.result !== undefined}
					<pre class="mt-1 max-h-40 overflow-auto rounded-box bg-base-200 p-2 text-xs">{JSON.stringify(call.outcome.result, null, 2)}</pre>
				{/if}
			</div>
		{:else}
			<p class="text-base-content/60">{t('agents-debug-no-subagents')}</p>
		{/each}
	</section>

	<section>
		<h4 class="mb-1 font-semibold">{t('agents-debug-tools')}</h4>
		{#each debug.tool_calls as call, i (i)}
			<div class="flex items-center gap-2">
				<span class="font-mono">{call.tool}</span>
				<span class="badge badge-sm {call.decision === 'allowed' ? 'badge-success' : 'badge-error'}">{call.decision}</span>
				<span class="text-xs text-base-content/60">{call.policy}</span>
			</div>
		{:else}
			<p class="text-base-content/60">{t('agents-debug-no-tools')}</p>
		{/each}
	</section>
</div>
