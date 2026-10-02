<script lang="ts">
	import { targetKindOf } from '$lib/agent-canvas';
	import { renameRoute, slotInfos, type AgentSummary, type Spec, type SpecIssue } from '$lib/agents';
	import { t } from '$lib/i18n.svelte';
	import BindsEditor from './BindsEditor.svelte';
	import CondEditor from './CondEditor.svelte';
	import FieldIssues from './FieldIssues.svelte';

	/**
	 * One route's editors, shared by the form builder (`all`) and the canvas,
	 * whose gate, route and target nodes each open their own part: `gate` the
	 * hard condition, `route` the name and description, `target` what the
	 * route opens (sub-agent, task, binds) or a generic note for a kind the
	 * builder has no editor for.
	 */
	let { spec = $bindable(), name, part = 'all', issues, agents, onrenamed, onremove }: {
		spec: Spec;
		name: string;
		part?: 'all' | 'gate' | 'route' | 'target';
		issues: SpecIssue[];
		agents: AgentSummary[];
		onrenamed?: (name: string) => void;
		onremove?: () => void;
	} = $props();

	const route = $derived(spec.routes[name] as Spec);
	const slots = $derived(slotInfos(spec));
	const slotNames = $derived(slots.map((s) => s.name));
	const targetKey = $derived(targetKindOf(route));

	function rename(to: string) {
		const next = renameRoute(spec, name, to);
		if (next !== name) onrenamed?.(next);
	}
	function agentLabel(a: AgentSummary): string {
		return `${a.display || a.name} — ${a.live_version === null ? t('agents-route-unpublished') : `v${a.live_version}`}`;
	}
	function agentOf(id: string | undefined): AgentSummary | undefined {
		return agents.find((a) => a.id === id);
	}
</script>

{#if route}
	<div class="space-y-3">
		{#if part === 'all' || part === 'route'}
			<div class="flex flex-wrap items-end gap-3">
				<label class="flex flex-col gap-1">
					<span class="label-text">{t('agents-route-name')}</span>
					<input class="input input-sm w-44 font-mono" value={name} onchange={(e) => rename(e.currentTarget.value)} />
				</label>
				<label class="flex min-w-60 grow flex-col gap-1">
					<span class="label-text">{t('agents-route-description')}</span>
					<input class="input input-sm w-full" bind:value={route.description} placeholder={t('agents-route-description-hint')} />
					<FieldIssues {issues} path="routes.{name}.description" />
				</label>
				{#if part === 'all' && onremove}
					<button class="btn btn-ghost btn-sm" type="button" onclick={onremove} aria-label={t('agents-remove')}>✕</button>
				{/if}
			</div>
			<FieldIssues {issues} path="routes.{name}" />
		{/if}

		{#if part === 'all' || part === 'gate'}
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
		{/if}

		{#if part === 'all' || part === 'target'}
			{#if targetKey === 'human'}
				<p class="text-sm text-base-content/70">{t('agents-route-human')}</p>
				<FieldIssues {issues} path="routes.{name}.human" />
			{:else if targetKey !== 'agent'}
				<p class="text-sm text-base-content/70">{t('agents-route-other-kind', { kind: targetKey })}</p>
				<FieldIssues {issues} path="routes.{name}.{targetKey}" />
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
		{/if}
	</div>
{/if}
