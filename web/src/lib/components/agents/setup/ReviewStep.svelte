<script lang="ts">
	import { base } from '$app/paths';
	import type { Spec } from '$lib/agents';
	import { SECTIONS, checklist, summary, type StepKey } from '$lib/agent-setup';
	import { useWorkspace } from '$lib/agent-workspace.svelte';
	import { t } from '$lib/i18n.svelte';
	import SetupChecklist from './SetupChecklist.svelte';

	/** The last step: the whole setup in plain sentences, what is still open, and the way to the test chat. */
	let { spec = $bindable(), onfix }: { spec: Spec; onfix: (step: StepKey | null) => void } = $props();
	const ws = useWorkspace();

	const ctx = $derived({ tr: t, grants: ws.detail?.grants ?? [], resources: ws.resources, agents: ws.agents });
	const todos = $derived(checklist(spec, ws.dirty ? [] : (ws.detail?.publish_issues ?? [])));
	const name = $derived(spec.profile?.display || ws.detail?.display || ws.detail?.name || '');
</script>

<div class="flex flex-col gap-5">
	<p class="m-0 text-base-content/70">{t('agents-setup-review-lead', { name })}</p>
	<dl class="card m-0 grid gap-x-4 gap-y-2 p-4 text-sm sm:grid-cols-[auto_minmax(0,1fr)]">
		{#each SECTIONS as step (step)}
			<dt class="font-semibold">{t(`agents-setup-step-${step}`)}</dt>
			<dd class="m-0 break-words text-base-content/80">{summary(step, spec, ctx)}</dd>
		{/each}
	</dl>
	<section class="card flex flex-col gap-3 p-4">
		<h3 class="m-0 text-base font-semibold">{t('agents-setup-checklist')}</h3>
		<SetupChecklist {todos} {spec} {onfix} />
	</section>
	<a class="link link-primary text-sm" href="{base}/agents/{ws.id}?tab=try">{t('agents-setup-review-try')}</a>
</div>
