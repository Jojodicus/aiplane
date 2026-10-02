<script lang="ts">
	import { addRoute, removeRoute, type AgentSummary, type Spec, type SpecIssue } from '$lib/agents';
	import { t } from '$lib/i18n.svelte';
	import RouteEditor from './RouteEditor.svelte';
	import RouterFields from './RouterFields.svelte';

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
</script>

<div class="space-y-4">
	<RouterFields bind:spec {issues} {pools} />

	{#each Object.keys(spec.routes) as name (name)}
		<div class="card card-border">
			<div class="card-body gap-3 p-4">
				<RouteEditor bind:spec {name} {issues} {agents} onremove={() => removeRoute(spec, name)} />
			</div>
		</div>
	{:else}
		<p class="text-sm text-base-content/60">{t('agents-routes-empty')}</p>
	{/each}
	<button class="btn btn-sm" type="button" onclick={() => addRoute(spec)}>+ {t('agents-routes-add')}</button>
</div>
