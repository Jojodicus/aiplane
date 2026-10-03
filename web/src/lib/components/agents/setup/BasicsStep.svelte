<script lang="ts">
	import ChipToggle from '$lib/components/ui/ChipToggle.svelte';
	import SegmentedControl from '$lib/components/ui/SegmentedControl.svelte';
	import type { Spec } from '$lib/agents';
	import {
		ANSWER_LANGUAGES,
		TIERS,
		TONES,
		defaultChatPool,
		hasTiers,
		humanize,
		readBasics,
		responseText,
		tierOf,
		writeBasics,
		type AnswerLanguage,
		type Tier
	} from '$lib/agent-setup';
	import { useWorkspace } from '$lib/agent-workspace.svelte';
	import { LOCALE_NAMES, t } from '$lib/i18n.svelte';
	import ImproveText from './ImproveText.svelte';
	import SuggestionBox from './SuggestionBox.svelte';
	import { writeOnChange } from './write-on-change.svelte';

	/** Task & tone: name, what the agent does, how it sounds, which language, how thorough (its pool). */
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

	const tiers = $derived(ws.resources?.tiers);
	const mapped = $derived(hasTiers(tiers));
	const tierOptions = $derived(TIERS.filter((tier) => !!tiers?.[tier]).map((tier) => ({ value: tier, label: t(`agents-setup-model-${tier}`) })));
	const grantedPools = $derived(ws.grants.filter((g) => g.kind === 'pool').map((g) => g.ref));
	const poolOptions = $derived([...new Set([...(ws.resources?.pools ?? []), ...grantedPools, ...(model.pool ? [model.pool] : [])])]);
	const holdable = (pool: string) => (ws.resources?.pools ?? []).includes(pool) || grantedPools.includes(pool);

	let modelError = $state<string | null>(null);

	function choosePool(pool: string, label: string) {
		modelError = null;
		if (!pool || pool === model.pool) return;
		if (!holdable(pool)) {
			modelError = t('agents-setup-model-unavailable', { choice: label });
			return;
		}
		const previous = model.pool;
		ws.stageGrant('pool', pool);
		model.pool = pool;
		if (previous) ws.stageRevoke('pool', previous);
	}
	const tierLabel = (tier: Tier) => t(`agents-setup-model-${tier}`);

	/** A new agent starts on the gateway's default chat model, staged for granting like any choice. */
	$effect(() => {
		const start = defaultChatPool(ws.resources);
		if (start && !model.pool && ws.writable) choosePool(start, start);
	});

	/** A proposed or improved tone is a response text; read it back into chips, language and the rest. */
	function adoptResponse(response: string) {
		const read = readBasics({ main: { instructions: { response } } });
		model.tones = read.tones;
		model.language = read.language;
		model.extra = read.extra;
		if (read.language && read.language !== 'visitor') fixedLanguage = read.language;
	}
	const suggested = $derived(ws.suggestion?.steps);
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
		<SuggestionBox part="tone" label={t('agents-setup-suggest-tone')} onapply={() => adoptResponse(suggested?.tone?.response ?? '')}>
			{#if suggested.tone.chips.length}
				<div class="mb-1.5 flex flex-wrap gap-1.5">
					{#each suggested.tone.chips as chip (chip)}<span class="badge badge-outline">{chip}</span>{/each}
				</div>
			{/if}
			<p class="m-0 whitespace-pre-line">{suggested.tone.response}</p>
		</SuggestionBox>
	{/if}
	<ImproveText field="tone" text={responseText(model)} onapply={adoptResponse} />

	<div class="flex flex-col gap-2">
		<span class="font-semibold">{t('agents-setup-model')}</span>
		{#if mapped}
			<span class="text-sm text-base-content/60">{t('agents-setup-model-hint')}</span>
			<SegmentedControl
				label={t('agents-setup-model')}
				options={tierOptions}
								bind:value={() => tierOf(model.pool, tiers) ?? ('' as Tier), (tier) => choosePool(tiers?.[tier] ?? '', tierLabel(tier))}
			/>
			{#if model.pool && !tierOf(model.pool, tiers)}
				<span class="text-sm text-base-content/60">{t('agents-setup-model-custom', { pool: humanize(model.pool.replace(/-/g, '_')) })}</span>
			{/if}
		{:else if poolOptions.length}
			<span class="text-sm text-base-content/60">{t('agents-setup-model-unmapped')}</span>
			<select
				class="select w-full max-w-sm"
				aria-label={t('agents-setup-model-pool')}
								value={model.pool}
				onchange={(e) => choosePool(e.currentTarget.value, e.currentTarget.value)}
			>
				<option value="" disabled>{t('agents-pick')}</option>
				{#each poolOptions as pool (pool)}<option value={pool}>{humanize(pool.replace(/-/g, '_'))}</option>{/each}
			</select>
		{:else}
			<div class="alert alert-warning text-sm"><span>{t('agents-setup-model-none')}</span></div>
		{/if}
		{#if modelError}<div class="alert alert-error text-sm" role="alert"><span>{modelError}</span></div>{/if}
	</div>
</div>
