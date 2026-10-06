<script lang="ts">
	import { onMount } from 'svelte';
	import { agentsApi, CHANNEL_KINDS, type AgentError, type ChannelKind, type NotifyChannel } from '#lib/agents.js';
	import { LOCALES, LOCALE_NAMES, locale, t, type Locale } from '#lib/i18n.svelte.js';

	/**
	 * Slack and Discord incoming webhooks that announce the agent's waiting
	 * turns, besides Web Push to whoever may answer. The webhook URL is a
	 * credential: it is sealed on the server and never shown again, only its
	 * host. With `details` off a message carries only the agent, the kind and
	 * the inbox link.
	 */
	let { agentId, writable, onchannels = () => {} }: { agentId: string; writable: boolean; onchannels?: (channels: NotifyChannel[]) => void } = $props();

	let channels = $state<NotifyChannel[]>([]);
	let kind = $state<ChannelKind>('slack');
	let name = $state('');
	let url = $state('');
	let details = $state(false);
	let lang = $state<Locale>(locale.current);
	let error = $state<string | null>(null);
	let busy = $state(false);

	async function reload() {
		channels = await agentsApi.channels(agentId);
		onchannels(channels);
	}

	async function run(action: () => Promise<unknown>) {
		busy = true;
		error = null;
		try {
			await action();
			await reload();
		} catch (err) {
			error = (err as AgentError).message;
		} finally {
			busy = false;
		}
	}

	onMount(() => void run(async () => {}));

	const add = () =>
		run(async () => {
			await agentsApi.addChannel(agentId, { kind, name: name.trim(), url: url.trim(), details, lang });
			name = '';
			url = '';
			details = false;
		});
</script>

<section class="card card-border">
	<div class="card-body gap-4 p-4">
		<h2 class="card-title text-base">{t('agents-channels-heading')}</h2>
		<p class="text-sm text-base-content/70">{t('agents-channels-intro')}</p>
		{#if error}<div class="alert alert-error text-sm" role="alert"><span>{error}</span></div>{/if}

		<ul class="flex flex-col divide-y divide-base-300">
			{#each channels as c (c.id)}
				<li class="flex flex-wrap items-center gap-2 py-2">
					<span class="badge badge-outline">{t(`agents-channels-kind-${c.kind}`)}</span>
					<span class="font-semibold">{c.name}</span>
					<span class="font-mono text-xs text-base-content/60 break-all">{c.url_host}</span>
					<span class="badge badge-ghost badge-sm">{c.lang}</span>
					{#if c.details}<span class="badge badge-warning badge-sm">{t('agents-channels-details-on')}</span>{/if}
					{#if writable}
						<button class="btn btn-ghost btn-xs ml-auto" type="button" disabled={busy} onclick={() => run(() => agentsApi.removeChannel(agentId, c.id))}>{t('agents-channels-remove')}</button>
					{/if}
				</li>
			{:else}
				<li class="py-2 text-sm text-base-content/60">{t('agents-channels-empty')}</li>
			{/each}
		</ul>

		{#if writable}
			<form class="flex flex-col gap-3" onsubmit={(e) => { e.preventDefault(); void add(); }}>
				<div class="flex flex-wrap items-end gap-3">
					<label class="flex flex-col gap-1">
						<span class="label-text">{t('agents-channels-kind')}</span>
						<select class="select w-32" bind:value={kind}>
							{#each CHANNEL_KINDS as k (k)}<option value={k}>{t(`agents-channels-kind-${k}`)}</option>{/each}
						</select>
					</label>
					<label class="flex flex-col gap-1">
						<span class="label-text">{t('agents-channels-name')}</span>
						<input class="input w-48 max-w-full" bind:value={name} required />
					</label>
					<label class="flex flex-col gap-1">
						<span class="label-text">{t('agents-channels-lang')}</span>
						<select class="select w-36" bind:value={lang}>
							{#each LOCALES as l (l)}<option value={l}>{LOCALE_NAMES[l]}</option>{/each}
						</select>
					</label>
				</div>
				<label class="flex flex-col gap-1">
					<span class="label-text">{t('agents-channels-url')}</span>
					<input class="input w-full font-mono" type="url" bind:value={url} required autocomplete="off" />
					<span class="text-xs text-base-content/60">{t('agents-channels-url-help')}</span>
				</label>
				<label class="flex items-start gap-2">
					<input class="checkbox checkbox-sm mt-0.5" type="checkbox" bind:checked={details} />
					<span class="text-sm">{t('agents-channels-details')}</span>
				</label>
				<div><button class="btn btn-primary" type="submit" disabled={busy || !name.trim() || !url.trim()}>{t('agents-channels-add')}</button></div>
			</form>
		{/if}
	</div>
</section>
