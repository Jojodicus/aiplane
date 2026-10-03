<script lang="ts">
	import { onMount } from 'svelte';
	import { base } from '$app/paths';
	import { agentsApi, embedSnippet, type AgentError, type EmbedKey, type Spec } from '$lib/agents';
	import { originOf, readColor, readSite, readVoice, voiceMissing, writeColor, writeSite, writeVoice, type Voice } from '$lib/agent-setup';
	import { useWorkspace } from '$lib/agent-workspace.svelte';
	import { t } from '$lib/i18n.svelte';
	import WidgetPreview from './WidgetPreview.svelte';
	import { writeOnChange } from './write-on-change.svelte';

	/**
	 * Where the agent appears: the websites allowed to embed it
	 * (`publish.origins`) and, once they are named, an embed key for them
	 * with the line to paste. The key is the server's to hand out, once.
	 * Then how the widget looks (`profile.color`) and whether visitors may
	 * talk to it and hear it (`publish.voice`), each direction on a pool the
	 * agent is granted.
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
	const missing = $derived(voiceMissing(voice));
	const offered = $derived(ws.resources?.voice_pools ?? { speech: [], transcription: [] });
	let voiceError = $state<string | null>(null);

	/** Grant the chosen pool to the agent first, then name it; the previous one is released if nothing live uses it. */
	async function choosePool(field: 'transcriptionPool' | 'speechPool', pool: string) {
		voiceError = null;
		const previous = voice[field];
		if (pool === previous) return;
		try {
			if (pool) await ws.ensureGrant('pool', pool);
			voice[field] = pool;
			if (previous && previous !== spec.main?.pool && !Object.values(voice).includes(previous)) await ws.releaseGrant('pool', previous);
		} catch (err) {
			voiceError = t('agents-setup-grant-failed', { reason: (err as AgentError).message });
		}
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
			snippet = embedSnippet(`${window.location.origin}${base}/embed.js`, created.key);
			keys = await agentsApi.embedKeys(ws.id);
		} catch (err) {
			error = (err as AgentError).message;
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
		{#each [{ dir: 'input', field: 'transcriptionPool', kind: 'transcription' }, { dir: 'output', field: 'speechPool', kind: 'speech' }] as const as row (row.dir)}
			<div class="flex flex-col gap-2 sm:flex-row sm:items-center">
				<label class="flex flex-1 items-center gap-2">
					<input type="checkbox" class="toggle toggle-primary" bind:checked={voice[row.dir]} disabled={!ws.writable} />
					<span>{t(`agents-setup-voice-${row.dir}`)}</span>
				</label>
				{#if voice[row.dir]}
					{#if offered[row.kind].length || voice[row.field]}
						<label class="flex items-center gap-2">
							<span class="text-sm">{t(`agents-setup-voice-${row.kind}-pool`)}</span>
							<select class="select select-sm w-48" value={voice[row.field]} onchange={(e) => void choosePool(row.field, e.currentTarget.value)} disabled={!ws.writable} aria-invalid={missing.includes(row.kind)}>
								<option value="">{t('agents-pick')}</option>
								{#each [...new Set([...offered[row.kind], ...(voice[row.field] ? [voice[row.field]] : [])])] as pool (pool)}<option value={pool}>{pool}</option>{/each}
							</select>
						</label>
					{:else}
						<span class="text-sm text-warning">{t('agents-setup-voice-no-pool')}</span>
					{/if}
				{/if}
			</div>
		{/each}
		{#if voice.output}
			<label class="flex flex-col gap-1">
				<span class="text-sm">{t('agents-setup-voice-voice')}</span>
				<input class="input input-sm w-48" bind:value={voice.voice} placeholder="alloy" disabled={!ws.writable} />
				<span class="text-xs text-base-content/60">{t('agents-setup-voice-voice-hint')}</span>
			</label>
		{/if}
		{#if voiceError}<div class="alert alert-error text-sm" role="alert"><span>{voiceError}</span></div>{/if}
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
