<script lang="ts">
	import { untrack } from 'svelte';
	import ChipToggle from '$lib/components/ui/ChipToggle.svelte';
	import SegmentedControl from '$lib/components/ui/SegmentedControl.svelte';
	import type { Spec } from '$lib/agents';
	import {
		ANSWER_LANGUAGES,
		TONES,
		defaultOutOfReach,
		modelGrantFor,
		modelsInUse,
		readBasics,
		responseText,
		suggestedTone,
		writeBasics,
		type AnswerLanguage
	} from '$lib/agent-setup';
	import ModelPicker from '../ModelPicker.svelte';
	import { useWorkspace } from '$lib/agent-workspace.svelte';
	import { LOCALE_NAMES, t } from '$lib/i18n.svelte';
	import ImproveText from './ImproveText.svelte';
	import SuggestionBox from './SuggestionBox.svelte';
	import { writeOnChange } from './write-on-change.svelte';

	/** Task & tone: name, what the agent does, how it sounds, which language, and the model it runs on. */
	let { spec = $bindable() }: { spec: Spec } = $props();
	const ws = useWorkspace();

	const initial = readBasics(spec);
	let model = $state({ ...initial, name: initial.name || ws.detail?.display || '' });
	writeOnChange(() => $state.snapshot(model), (m) => writeBasics(spec, m));

	let fixedLanguage = $state<AnswerLanguage>(model.language && model.language !== 'visitor' ? model.language : 'en');
	const languageMode = $derived(model.language === null ? 'none' : model.language === 'visitor' ? 'visitor' : 'fixed');
	function setLanguageMode(mode: 'visitor' | 'fixed' | 'none') {
		model.language = mode === 'visitor' ? 'visitor' : mode === 'fixed' ? fixedLanguage : null;
	}

	const heldModels = $derived(ws.grants.filter((g) => g.kind === 'model').map((g) => g.ref));
	const fallback = $derived(ws.resources?.defaults?.chat ?? null);
	const unheldDefault = $derived(defaultOutOfReach('chat', model.model, heldModels, ws.resources));
	const nothingToPick = $derived(!fallback && !model.model && !heldModels.length && !(ws.resources?.models?.chat?.length ?? 0));

	/** Stage the grant of what the agent will run on; what it ran on before is staged for revoking unless the spec still uses it. Both are made on save. */
	function chooseModel(next: string) {
		if (next === model.model) return;
		const previous = model.model || fallback;
		model.model = next;
		const grant = modelGrantFor('chat', next, ws.resources);
		if (grant) ws.stageGrant('model', grant);
		const after = { ...$state.snapshot(spec), main: { ...(spec.main ?? {}), model: next || undefined } };
		if (previous && previous !== grant && !modelsInUse(after, ws.resources?.defaults).includes(previous)) ws.stageRevoke('model', previous);
	}

	/** An agent on the default starts with the default's grant staged, like any choice. */
	let started = false;
	$effect(() => {
		if (started || !ws.resources || !ws.writable) return;
		started = true;
		untrack(() => {
			const grant = model.model ? null : modelGrantFor('chat', '', ws.resources);
			if (grant) ws.stageGrant('model', grant);
		});
	});

	/** A proposed or improved tone is a response text; read it back into chips, language and the rest. */
	function adoptResponse(response: string) {
		const read = readBasics({ main: { instructions: { response } } });
		model.tones = read.tones;
		model.language = read.language;
		model.extra = read.extra;
		if (read.language && read.language !== 'visitor') fixedLanguage = read.language;
	}
	function applyTone() {
		const tone = suggested?.tone;
		if (!tone) return;
		const next = suggestedTone(tone, model);
		model.tones = next.tones;
		model.language = next.language;
		model.extra = next.extra;
		if (next.language && next.language !== 'visitor') fixedLanguage = next.language;
	}
	const suggested = $derived(ws.suggestion?.steps);
	const suggestedLanguage = $derived.by(() => {
		const lang = suggested?.tone?.language;
		if (lang === 'visitor') return t('agents-setup-language-visitor');
		return lang && (ANSWER_LANGUAGES as readonly string[]).includes(lang) ? LOCALE_NAMES[lang as AnswerLanguage] : null;
	});
</script>

<div class="flex flex-col gap-5">
	<label class="flex flex-col gap-1">
		<span class="font-semibold">{t('agents-setup-name')}</span>
		<input class="input w-full" bind:value={model.name} maxlength="120" />
	</label>

	<label class="flex flex-col gap-1">
		<span class="font-semibold">{t('agents-setup-task')}</span>
		<span class="text-sm text-base-content/60">{t('agents-setup-task-hint')}</span>
		<textarea class="textarea min-h-28 w-full" bind:value={model.task}></textarea>
	</label>
	{#if suggested?.task}
		<SuggestionBox part="task" label={t('agents-setup-suggest-task')} onapply={() => { model.task = suggested?.task?.orchestration ?? model.task; }}>
			<p class="m-0 whitespace-pre-line">{suggested.task.orchestration}</p>
		</SuggestionBox>
	{/if}
	<ImproveText field="task" text={model.task} onapply={(s) => (model.task = s)} />

	<div class="flex flex-col gap-2">
		<span class="font-semibold">{t('agents-setup-tone')}</span>
		<div class="flex flex-wrap gap-2">
			{#each TONES as tone (tone)}
				<ChipToggle
					label={t(`agents-setup-tone-${tone}`)}
					bind:selected={() => model.tones.includes(tone), (on) => (model.tones = on ? [...model.tones, tone] : model.tones.filter((x) => x !== tone))}
				/>
			{/each}
		</div>
	</div>

	<div class="flex flex-col gap-2">
		<span class="font-semibold">{t('agents-setup-language')}</span>
		<div class="flex flex-wrap items-center gap-2">
			<SegmentedControl
				label={t('agents-setup-language')}
				options={[
					{ value: 'visitor', label: t('agents-setup-language-visitor') },
					{ value: 'fixed', label: t('agents-setup-language-fixed') },
					{ value: 'none', label: t('agents-setup-language-none') }
				]}
				bind:value={() => languageMode, setLanguageMode}
			/>
			{#if languageMode === 'fixed'}
				<select
					class="select w-44"
					aria-label={t('agents-setup-language-pick')}
					bind:value={() => fixedLanguage, (v) => ((fixedLanguage = v), (model.language = v))}
				>
					{#each ANSWER_LANGUAGES as lang (lang)}<option value={lang}>{LOCALE_NAMES[lang]}</option>{/each}
				</select>
			{/if}
		</div>
	</div>

	<label class="flex flex-col gap-1">
		<span class="font-semibold">{t('agents-setup-tone-more')}</span>
		<span class="text-sm text-base-content/60">{t('agents-setup-tone-more-hint')}</span>
		<textarea class="textarea w-full" rows="2" bind:value={model.extra}></textarea>
	</label>
	{#if suggested?.tone}
		<SuggestionBox part="tone" label={t('agents-setup-suggest-tone')} onapply={applyTone}>
			{#if suggested.tone.chips.length || suggestedLanguage}
				<div class="mb-1.5 flex flex-wrap gap-1.5">
					{#each suggested.tone.chips as chip (chip)}<span class="badge badge-outline">{t(`agents-setup-tone-${chip}`)}</span>{/each}
					{#if suggestedLanguage}<span class="badge badge-outline badge-primary">{suggestedLanguage}</span>{/if}
				</div>
			{/if}
			{#if suggested.tone.response}<p class="m-0 whitespace-pre-line">{suggested.tone.response}</p>{/if}
		</SuggestionBox>
	{/if}
	<ImproveText field="tone" text={responseText(model)} onapply={adoptResponse} />

	<div class="flex flex-col gap-2">
		<span class="font-semibold">{t('agents-setup-model')}</span>
		<span class="text-sm text-base-content/60">{t('agents-setup-model-hint')}</span>
		{#if nothingToPick}
			<div class="alert alert-warning text-sm"><span>{t('agents-setup-model-none')}</span></div>
		{:else}
			<ModelPicker
				kind="chat"
				value={model.model}
				held={heldModels}
				resources={ws.resources}
				emptyLabel={fallback ? t('agents-setup-model-default', { model: fallback }) : null}
				ariaLabel={t('agents-setup-model')}
				disabled={!ws.writable}
				onchange={chooseModel}
			/>
		{/if}
		{#if unheldDefault}<div class="alert alert-warning text-sm" role="alert"><span>{t('agents-setup-model-default-unheld', { model: unheldDefault })}</span></div>{/if}
	</div>
</div>