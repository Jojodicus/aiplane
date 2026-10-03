<script lang="ts">
	import ChoiceCard from '$lib/components/ui/ChoiceCard.svelte';
	import { agentsApi, cleanSpec, ensureShape, type AgentError, type Spec } from '$lib/agents';
	import { STEPS, TEMPLATES, applyTemplate, isBlank, setupErrorMessage, type StepKey, type TemplateKey } from '$lib/agent-setup';
	import { useWorkspace } from '$lib/agent-workspace.svelte';
	import { t } from '$lib/i18n.svelte';

	/**
	 * Where the assistant starts: a template, each a valid starter draft
	 * (`agent-templates.json`), and/or a scenario the prompt assistant
	 * (#117, `assist/suggest`) turns into a proposal. The proposal is kept on
	 * the workspace and each later step offers its part to apply; what the
	 * assistant left out is listed here with its reason.
	 */
	let { spec = $bindable() }: { spec: Spec } = $props();
	const ws = useWorkspace();

	let scenario = $state('');
	let chosen = $state<TemplateKey | null>(null);
	let applied = $state(false);
	let proposing = $state(false);
	let proposeError = $state<string | null>(null);

	/** Which step a part of the proposal belongs to, for the list of what was left out. */
	const PART_STEP: Record<string, StepKey> = {
		task: 'basics',
		tone: 'basics',
		scope: 'scope',
		abilities: 'abilities',
		slots: 'slots',
		identity: 'identity',
		handoffs: 'routes',
		tests: 'review'
	};
	const stepTitle = (part: string) => {
		const step = PART_STEP[part];
		return step && STEPS.includes(step) ? t(`agents-setup-step-${step}`) : part;
	};

	async function propose() {
		proposing = true;
		proposeError = null;
		try {
			ws.propose(
				await agentsApi.suggest(ws.id, {
					scenario: scenario.trim(),
					...(chosen && chosen !== 'blank' ? { template: chosen } : {}),
					current_draft: cleanSpec(spec)
				})
			);
		} catch (err) {
			proposeError = setupErrorMessage(err as AgentError, t, 'assist');
		} finally {
			proposing = false;
		}
	}

	function choose(key: TemplateKey) {
		if (!isBlank(spec) && !confirm(t('agents-setup-tpl-replace', { template: t(`agents-setup-tpl-${key}`) }))) return;
		spec = ensureShape(applyTemplate(spec, key, t));
		chosen = key;
		applied = true;
	}
</script>

<div class="flex flex-col gap-5">
	<p class="m-0 text-base-content/70">{t('agents-setup-start-lead')}</p>

	<label class="flex flex-col gap-1">
		<span class="font-semibold">{t('agents-setup-scenario')}</span>
		<textarea class="textarea min-h-24 w-full" bind:value={scenario} placeholder={t('agents-setup-scenario-placeholder')}></textarea>
	</label>
	<div class="flex flex-wrap items-center gap-3">
		<button class="btn btn-primary" type="button" disabled={proposing || !scenario.trim() || !ws.writable} onclick={() => void propose()}>
			{#if proposing}<span class="loading loading-spinner loading-sm"></span>{:else}<span aria-hidden="true">✦</span>{/if}
			{t('agents-setup-propose')}
		</button>
		<span class="text-sm text-base-content/60">{t('agents-setup-or-template')}</span>
	</div>
	{#if proposing}<p class="m-0 text-sm text-base-content/70" role="status">{t('agents-setup-proposing')}</p>{/if}
	{#if proposeError}<div class="alert alert-error text-sm" role="alert"><span>{proposeError}</span></div>{/if}
	{#if ws.suggestion}
		<div class="alert alert-info flex-col items-start text-sm" role="status">
			<span>{t('agents-setup-proposal-ready')}</span>
			{#if ws.suggestion.dropped.length}
				<span class="font-semibold">{t('agents-setup-dropped')}</span>
				<ul class="m-0 list-inside list-disc">
					{#each ws.suggestion.dropped as d, i (i)}
						<li>{stepTitle(d.step)}{d.item ? ` · ${d.item}` : ''}: {d.reason}</li>
					{/each}
				</ul>
			{/if}
		</div>
	{/if}

	<div class="grid gap-2.5 sm:grid-cols-2 lg:grid-cols-3" role="radiogroup" aria-label={t('agents-setup-or-template')}>
		{#each TEMPLATES as key (key)}
			<ChoiceCard title={t(`agents-setup-tpl-${key}`)} description={t(`agents-setup-tpl-${key}-desc`)} selected={chosen === key} onselect={() => choose(key)} />
		{/each}
	</div>
	{#if applied}
		<div class="alert alert-success text-sm" role="status"><span>{t('agents-setup-tpl-applied')}</span></div>
	{/if}
</div>
