<script lang="ts">
	import { onMount } from 'svelte';
	import { base } from '$app/paths';
	import { agentsApi, embedSnippet, type AgentError, type EmbedKey, type Spec } from '$lib/agents';
	import { originOf, readSite, writeSite } from '$lib/agent-setup';
	import { useWorkspace } from '$lib/agent-workspace.svelte';
	import { t } from '$lib/i18n.svelte';
	import WidgetPreview from './WidgetPreview.svelte';
	import { writeOnChange } from './write-on-change.svelte';

	/**
	 * Where the agent appears: the websites allowed to embed it
	 * (`publish.origins`) and, once they are named, an embed key for them
	 * with the line to paste. The key is the server's to hand out, once.
	 */
	let { spec = $bindable() }: { spec: Spec } = $props();
	const ws = useWorkspace();

	let text = $state(readSite(spec).join('\n'));
	const lines = $derived(text.split('\n').map((l) => l.trim()).filter(Boolean));
	const invalid = $derived(lines.filter((l) => !originOf(l)));
	const origins = $derived([...new Set(lines.map(originOf).filter((o): o is string => !!o))]);
	writeOnChange(() => lines, (m) => writeSite(spec, m));

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
