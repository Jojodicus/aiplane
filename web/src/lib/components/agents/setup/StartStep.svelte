<script lang="ts">
	import ChoiceCard from '$lib/components/ui/ChoiceCard.svelte';
	import { ensureShape, type Spec } from '$lib/agents';
	import { TEMPLATES, applyTemplate, isBlank, type TemplateKey } from '$lib/agent-setup';
	import { t } from '$lib/i18n.svelte';

	/**
	 * Where the assistant starts: a template, each a valid starter draft
	 * (`agent-templates.json`), and the scenario a later prompt assistant
	 * (#117) will turn into a proposal. Until that exists its button is
	 * shown, disabled, with what it will do — nothing pretends to run.
	 */
	let { spec = $bindable() }: { spec: Spec } = $props();

	let scenario = $state('');
	let chosen = $state<TemplateKey | null>(null);
	let applied = $state(false);

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
		<span class="tooltip" data-tip={t('agents-setup-propose-coming')}>
			<button class="btn btn-primary" type="button" disabled aria-describedby="propose-coming">✦ {t('agents-setup-propose')}</button>
		</span>
		<span id="propose-coming" class="sr-only">{t('agents-setup-propose-coming')}</span>
		<span class="text-sm text-base-content/60">{t('agents-setup-or-template')}</span>
	</div>

	<div class="grid gap-2.5 sm:grid-cols-2 lg:grid-cols-3" role="radiogroup" aria-label={t('agents-setup-or-template')}>
		{#each TEMPLATES as key (key)}
			<ChoiceCard title={t(`agents-setup-tpl-${key}`)} description={t(`agents-setup-tpl-${key}-desc`)} selected={chosen === key} onselect={() => choose(key)} />
		{/each}
	</div>
	{#if applied}
		<div class="alert alert-success text-sm" role="status"><span>{t('agents-setup-tpl-applied')}</span></div>
	{/if}
</div>
