<script lang="ts">
	import { onMount } from 'svelte';
	import { agentsApi, embedSnippet, type AgentError, type EmbedKey, type Spec } from '#lib/agents.js';
	import {
		originOf,
		readColor,
		readSite,
		readVoice,
		defaultOutOfReach,
		modelGrantFor,
		modelsInUse,
		setupErrorMessage,
		speechVoices,
		voiceMissing,
		voiceOffered,
		writeColor,
		writeSite,
		writeVoice,
		type Voice,
		type VoiceKind
	} from '#lib/agent-setup.js';
	import { useWorkspace } from '#lib/agent-workspace.svelte.js';
	import { t } from '#lib/i18n.svelte.js';
	import ModelPicker from '../ModelPicker.svelte';
	import WidgetPreview from './WidgetPreview.svelte';
	import { writeOnChange } from './write-on-change.svelte';

	/**
	 * Where the agent appears: the websites allowed to embed it
	 * (`publish.origins`) and, once they are named, an embed key for them
	 * with the line to paste. The key is the server's to hand out, once.
	 * Then how the widget looks (`profile.color`) and whether visitors may
	 * talk to it and hear it (`publish.voice`), each direction on a model the
	 * agent is granted. Switching a direction on starts it on the gateway's
	 * default model for it (its grant staged, so the grant route's cap
	 * applies on save); a direction the manager may grant no model for is
	 * explained instead of offered.
	 */
	let { spec = $bindable() }: { spec: Spec } = $props();
	const ws = useWorkspace();

	let text = $state(readSite(spec).join('\n'));
	const lines = $derived(text.split('\n').map((l) => l.trim()).filter(Boolean));
	const invalid = $derived(lines.filter((l) => !originOf(l)));
	const origins = $derived([...new Set(lines.map(originOf).filter((o): o is string => !!o))]);
	writeOnChange(() => lines, (m) => writeSite(spec, m));

	let color = $state(readColor(spec));
	writeOnChange(() => color, (c) => writeColor(spec, c));

	const voice = $state<Voice>(readVoice(spec));
	writeOnChange(() => $state.snapshot(voice), (v) => writeVoice(spec, v));
	const defaults = $derived(ws.resources?.defaults);
	const missing = $derived(voiceMissing(voice, defaults));
	const heldModels = $derived(ws.grants.filter((g) => g.kind === 'model').map((g) => g.ref));
	type Field = 'transcriptionModel' | 'speechModel';

	function withVoice(v: Voice): Spec {
		const next = $state.snapshot(spec) as Spec;
		writeVoice(next, v);
		return next;
	}

	/** Stage the grant of what a direction now runs on; a model nothing uses any more is staged for revoking. Both are made on save. */
	function restage(before: Voice, field: Field, kind: VoiceKind) {
		const was = modelsInUse(withVoice(before), defaults);
		const now = modelsInUse(withVoice(voice), defaults);
		const on = field === 'transcriptionModel' ? voice.input : voice.output;
		const grant = on ? modelGrantFor(kind, voice[field], ws.resources) : null;
		if (grant) ws.stageGrant('model', grant);
		for (const model of was) if (!now.includes(model)) ws.stageRevoke('model', model);
	}

	function chooseModel(field: Field, kind: VoiceKind, model: string) {
		if (model === voice[field]) return;
		const before = $state.snapshot(voice);
		voice[field] = model;
		restage(before, field, kind);
	}

	function switchVoice(dir: 'input' | 'output', field: Field, kind: VoiceKind, on: boolean) {
		const before = $state.snapshot(voice);
		voice[dir] = on;
		restage(before, field, kind);
	}

	let keys = $state<EmbedKey[]>([]);
	let snippet = $state<string | null>(null);
	let error = $state<string | null>(null);
	let busy = $state(false);
	let copied = $state(false);

	onMount(async () => {
		try {
			keys = await agentsApi.embedKeys(ws.id);
		} catch {
			keys = [];
		}
	});

	async function createKey() {
		busy = true;
		error = null;
		try {
			const name = `${spec.profile?.display || ws.detail?.name || ''} · ${new URL(origins[0]).host}`;
			const created = await agentsApi.createEmbedKey(ws.id, { name, origins });
			snippet = embedSnippet(`${window.location.origin}/embed.js`, created.key);
			keys = await agentsApi.embedKeys(ws.id);
		} catch (err) {
			error = setupErrorMessage(err as AgentError, t);
		} finally {
			busy = false;
		}
	}

	async function copy() {
		if (!snippet) return;
		try {
			await navigator.clipboard.writeText(snippet);
			copied = true;
		} catch {
			copied = false;
		}
	}
	const activeKeys = $derived(keys.filter((k) => !k.revoked_at).length);
</script>

<div class="flex flex-col gap-5">
	<label class="flex flex-col gap-1">
		<span class="font-semibold">{t('agents-setup-site-origins')}</span>
		<span class="text-sm text-base-content/60">{t('agents-setup-site-origins-hint')}</span>
		<textarea class="textarea w-full font-mono" rows="2" bind:value={text} placeholder="https://www.example.com" aria-invalid={invalid.length > 0}></textarea>
		{#each invalid as value (value)}
			<span class="text-sm text-error">{t('agents-setup-site-invalid', { value })}</span>
		{/each}
	</label>

	<div class="flex flex-col gap-1">
		<span class="font-semibold">{t('agents-setup-site-color')}</span>
		<span class="text-sm text-base-content/60">{t('agents-setup-site-color-hint')}</span>
		<div class="flex items-center gap-2">
			<input type="color" class="h-10 w-14 cursor-pointer rounded-field border border-base-300 bg-base-100" value={color || '#6c3eb5'} oninput={(e) => (color = e.currentTarget.value)} aria-label={t('agents-setup-site-color')} />
			<input class="input w-32 font-mono" bind:value={color} placeholder="#6c3eb5" aria-label={t('agents-setup-site-color')} />
			{#if color}<button class="btn btn-ghost btn-sm" type="button" onclick={() => (color = '')}>{t('agents-setup-site-color-clear')}</button>{/if}
		</div>
	</div>

	<fieldset class="flex flex-col gap-3">
		<legend class="font-semibold">{t('agents-setup-voice')}</legend>
		<span class="text-sm text-base-content/60">{t('agents-setup-voice-hint')}</span>
		{#each [{ dir: 'input', field: 'transcriptionModel', kind: 'transcription' }, { dir: 'output', field: 'speechModel', kind: 'speech' }] as const as row (row.dir)}
			{@const available = voiceOffered(row.kind, voice[row.field], ws.resources)}
			{@const fallback = defaults?.[row.kind] ?? null}
			{#if !available}
				<div class="alert alert-info text-sm" role="status"><span>{t(`agents-setup-voice-unavailable-${row.dir}`)}</span></div>
			{/if}
			{#if available || voice[row.dir]}
				<div class="flex flex-col gap-2 sm:flex-row sm:items-center">
					<label class="flex flex-1 items-center gap-2">
						<input
							type="checkbox"
							class="toggle toggle-primary"
							checked={voice[row.dir]}
							onchange={(e) => switchVoice(row.dir, row.field, row.kind, e.currentTarget.checked)}
							disabled={!ws.writable}
						/>
						<span>{t(`agents-setup-voice-${row.dir}`)}</span>
					</label>
					{#if voice[row.dir] && available}
						<div class="flex items-center gap-2">
							<span class="text-sm" class:text-error={missing.includes(row.kind)}>{t(`agents-setup-voice-${row.kind}-model`)}</span>
							<ModelPicker
								kind={row.kind}
								value={voice[row.field]}
								held={heldModels}
								resources={ws.resources}
								emptyLabel={fallback ? t('agents-setup-model-default', { model: fallback }) : null}
								ariaLabel={t(`agents-setup-voice-${row.kind}-model`)}
								disabled={!ws.writable}
								class="w-56"
								onchange={(model) => chooseModel(row.field, row.kind, model)}
							/>
						</div>
					{/if}
				</div>
				{#if voice[row.dir] && defaultOutOfReach(row.kind, voice[row.field], heldModels, ws.resources)}
					<div class="alert alert-warning text-sm" role="alert"><span>{t('agents-setup-model-default-unheld', { model: fallback ?? '' })}</span></div>
				{/if}
			{/if}
		{/each}
		{#if voice.output}
			<label class="flex flex-col gap-1">
				<span class="text-sm">{t('agents-setup-voice-voice')}</span>
				<select class="select select-sm w-64" bind:value={voice.voice} disabled={!ws.writable}>
					<option value="">{t('agents-setup-voice-default')}</option>
					{#each speechVoices(voice, ws.resources) as name (name)}<option value={name}>{name}</option>{/each}
				</select>
				<span class="text-xs text-base-content/60">{t('agents-setup-voice-voice-hint')}</span>
			</label>
		{/if}
	</fieldset>

	<div class="flex flex-col gap-4 sm:flex-row sm:items-start">
		<WidgetPreview {spec} />
		<div class="flex min-w-0 flex-1 flex-col gap-2">
			<span class="font-semibold">{t('agents-setup-site-code')}</span>
			<span class="text-sm text-base-content/60">{t('agents-setup-site-code-hint')}</span>
			{#if error}<div class="alert alert-error text-sm" role="alert"><span>{error}</span></div>{/if}
			{#if snippet}
				<pre class="m-0 overflow-x-auto rounded-field bg-base-100 p-3 font-mono text-xs select-all">{snippet}</pre>
				<button class="btn btn-sm self-start" type="button" onclick={() => void copy()}>{copied ? t('agents-setup-copied') : t('agents-setup-copy')}</button>
			{:else}
				<button class="btn btn-sm self-start" type="button" disabled={busy || !origins.length || !ws.writable} onclick={() => void createKey()}>{t('agents-setup-site-create-key')}</button>
			{/if}
			{#if activeKeys}
				<span class="text-sm text-base-content/60">{t('agents-setup-site-keys', { count: activeKeys })}</span>
			{/if}
		</div>
	</div>
</div>
