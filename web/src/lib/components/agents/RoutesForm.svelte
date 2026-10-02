<script lang="ts">
	import { freshName, renameKey, slotInfos, splitList, type AgentSummary, type Spec, type SpecIssue } from '$lib/agents';
	import { t } from '$lib/i18n.svelte';
	import BindsEditor from './BindsEditor.svelte';
	import CondEditor from './CondEditor.svelte';
	import FieldIssues from './FieldIssues.svelte';

	/**
	 * Router and routes (`docs/agents.md` §3-4). A route is a hard gate plus
	 * the sub-agent it opens: the gate is the security boundary, the router
	 * only picks among routes whose gate already holds.
	 */
	let { spec = $bindable(), issues, agents, pools }: {
		spec: Spec;
		issues: SpecIssue[];
		agents: AgentSummary[];
		pools: string[];
	} = $props();

	const slots = $derived(slotInfos(spec));
	const slotNames = $derived(slots.map((s) => s.name));
	const router = $derived(spec.router ?? {});

	function setRouter(key: string, value: string) {
		spec.router ??= {};
		if (value) spec.router[key] = value;
		else delete spec.router[key];
		if (!Object.keys(spec.router).length) delete spec.router;
	}
	function rename(from: string, to: string) {
		const name = to.trim();
		if (name) spec.routes = renameKey(spec.routes, from, name);
	}
	function agentLabel(a: AgentSummary): string {
		return `${a.display || a.name} — ${a.live_version === null ? t('agents-route-unpublished') : `v${a.live_version}`}`;
	}
	function agentOf(id: string | undefined): AgentSummary | undefined {
		return agents.find((a) => a.id === id);
	}
</script>

<div class="space-y-4">
	<div class="flex flex-wrap items-end gap-3">
		<label class="flex flex-col gap-1">
			<span class="label-text">{t('agents-router-kind')}</span>
			<select class="select select-sm w-72" value={router.kind ?? ''} onchange={(e) => setRouter('kind', e.currentTarget.value)}>
				<option value="">{t('agents-router-default')}</option>
				<option value="rules">{t('agents-router-rules')}</option>
				<option value="classifier">{t('agents-router-classifier')}</option>
			</select>
			<FieldIssues {issues} path="router.kind" />
		</label>
		{#if router.kind === 'classifier'}
			<label class="flex flex-col gap-1">
				<span class="label-text">{t('agents-router-pool')}</span>
				<select class="select select-sm w-48" value={router.pool ?? ''} onchange={(e) => setRouter('pool', e.currentTarget.value)}>
					<option value="">{t('agents-router-pool-main')}</option>
					{#each pools as pool (pool)}<option value={pool}>{pool}</option>{/each}
				</select>
				<FieldIssues {issues} path="router.pool" />
			</label>
		{/if}
		{#if router.kind === 'rules'}
			<label class="flex min-w-60 flex-col gap-1">
				<span class="label-text">{t('agents-router-order')}</span>
				<input
					class="input input-sm w-full font-mono"
					value={(router.order ?? []).join(', ')}
					onchange={(e) => {
						spec.router.order = splitList(e.currentTarget.value);
						if (!spec.router.order.length) delete spec.router.order;
					}}
					placeholder={t('agents-router-order-hint')}
				/>
				<FieldIssues {issues} path="router.order" />
			</label>
		{/if}
	</div>

	{#each Object.entries(spec.routes as Record<string, Spec>) as [name, route] (name)}
		<div class="card card-border">
			<div class="card-body gap-3 p-4">
				<div class="flex flex-wrap items-end gap-3">
					<label class="flex flex-col gap-1">
						<span class="label-text">{t('agents-route-name')}</span>
						<input class="input input-sm w-44 font-mono" value={name} onchange={(e) => rename(name, e.currentTarget.value)} />
					</label>
					<label class="flex min-w-60 grow flex-col gap-1">
						<span class="label-text">{t('agents-route-description')}</span>
						<input class="input input-sm w-full" bind:value={route.description} placeholder={t('agents-route-description-hint')} />
						<FieldIssues {issues} path="routes.{name}.description" />
					</label>
					<button class="btn btn-ghost btn-sm" type="button" onclick={() => delete spec.routes[name]} aria-label={t('agents-remove')}>✕</button>
				</div>
				<FieldIssues {issues} path="routes.{name}" />

				<div>
					<span class="label-text">{t('agents-route-gate')}</span>
					<p class="mb-2 text-xs text-base-content/60">{t('agents-route-gate-hint')}</p>
					{#if route.when}
						<CondEditor bind:cond={route.when} {slots} path="routes.{name}.when" {issues} />
					{:else}
						<button class="btn btn-sm" type="button" onclick={() => (route.when = { slot: '', set: true })}>+ {t('agents-gate-add-condition')}</button>
						<FieldIssues {issues} path="routes.{name}.when" />
					{/if}
				</div>

				{#if route.human}
					<p class="text-sm text-base-content/70">{t('agents-route-human')}</p>
					<FieldIssues {issues} path="routes.{name}.human" />
				{:else}
					<label class="flex flex-col gap-1">
						<span class="label-text">{t('agents-route-agent')}</span>
						<select class="select select-sm w-full max-w-lg" bind:value={route.agent}>
							<option value="">{t('agents-pick')}</option>
							{#each agents as a (a.id)}<option value={a.id}>{agentLabel(a)}</option>{/each}
							{#if route.agent && !agentOf(route.agent)}<option value={route.agent}>{route.agent}</option>{/if}
						</select>
						{#if agentOf(route.agent) && agentOf(route.agent)?.live_version === null}
							<p class="text-xs text-warning">{t('agents-route-agent-unpublished')}</p>
						{/if}
						<FieldIssues {issues} path="routes.{name}.agent" />
					</label>
					<label class="flex flex-col gap-1">
						<span class="label-text">{t('agents-route-task')}</span>
						<textarea class="textarea min-h-16 w-full" bind:value={route.task} placeholder={t('agents-route-task-hint')}></textarea>
						<FieldIssues {issues} path="routes.{name}.task" />
					</label>
					<div>
						<span class="label-text">{t('agents-route-binds')}</span>
						<p class="mb-2 text-xs text-base-content/60">{t('agents-route-binds-hint')}</p>
						<BindsEditor
							bind={route.bind ?? {}}
							path="routes.{name}.bind"
							{issues}
							kinds={['state', 'const']}
							slots={slotNames}
							onchange={(bind) => (route.bind = bind)}
						/>
					</div>
				{/if}
			</div>
		</div>
	{:else}
		<p class="text-sm text-base-content/60">{t('agents-routes-empty')}</p>
	{/each}
	<button class="btn btn-sm" type="button" onclick={() => (spec.routes[freshName(spec.routes, 'route')] = { when: { slot: '', set: true }, agent: '', task: '' })}>+ {t('agents-routes-add')}</button>
</div>
