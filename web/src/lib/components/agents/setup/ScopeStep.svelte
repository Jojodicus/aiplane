<script lang="ts">
	import ChipToggle from '$lib/components/ui/ChipToggle.svelte';
	import type { Spec } from '$lib/agents';
	import { readScope, strictIncomplete, writeScope } from '$lib/agent-setup';
	import { t } from '$lib/i18n.svelte';
	import { writeOnChange } from './write-on-change.svelte';

	/**
	 * The topic list (#115 `scope`): what the agent talks about, the answer for
	 * everything else, and whether the topic guard enforces it.
	 */
	let { spec = $bindable() }: { spec: Spec } = $props();

	let model = $state(readScope(spec));
	writeOnChange(() => $state.snapshot(model), (m) => writeScope(spec, m));

	let draft = $state('');
	function addTopic() {
		const topic = draft.trim();
		if (topic && !model.topics.includes(topic)) model.topics = [...model.topics, topic];
		draft = '';
	}
</script>

<div class="flex flex-col gap-5">
	<p class="m-0 text-base-content/70">{t('agents-setup-scope-lead')}</p>

	<div class="flex flex-col gap-2">
		<span class="font-semibold">{t('agents-setup-topics')}</span>
		{#if model.topics.length}
			<div class="flex flex-wrap gap-2">
				{#each model.topics as topic (topic)}
					<ChipToggle label={topic} onremove={() => (model.topics = model.topics.filter((x) => x !== topic))} />
				{/each}
			</div>
		{/if}
		<form class="flex flex-wrap gap-2" onsubmit={(e) => { e.preventDefault(); addTopic(); }}>
			<input class="input min-w-0 flex-1" bind:value={draft} placeholder={t('agents-setup-topic-placeholder')} aria-label={t('agents-setup-topic-add')} />
			<button class="btn" type="submit" disabled={!draft.trim()}>{t('agents-setup-topic-add')}</button>
		</form>
	</div>

	<label class="flex flex-col gap-1">
		<span class="font-semibold">{t('agents-setup-refusal')}</span>
		<textarea class="textarea w-full" rows="2" bind:value={model.refusal}></textarea>
	</label>

	<label class="flex items-start gap-3">
		<input type="checkbox" class="toggle toggle-primary mt-0.5" bind:checked={model.strict} />
		<span class="flex flex-col gap-1">
			<span class="font-semibold">{t('agents-setup-strict')}</span>
			<span class="text-sm text-base-content/60">{t('agents-setup-strict-hint')}</span>
		</span>
	</label>
	{#if strictIncomplete(model)}
		<div class="alert alert-warning text-sm" role="status"><span>{t('agents-setup-strict-needs')}</span></div>
	{/if}

	<p class="m-0 text-sm text-base-content/60">{t('agents-setup-scope-try')}</p>
</div>
