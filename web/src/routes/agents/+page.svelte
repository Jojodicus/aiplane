<script lang="ts">
	import { onMount } from 'svelte';
	import { base } from '$app/paths';
	import { goto } from '$app/navigation';
	import { agentsApi, type AgentError, type AgentSummary } from '$lib/agents';
	import { locale, t } from '$lib/i18n.svelte';

	let agents = $state<AgentSummary[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	let dialog = $state<HTMLDialogElement>();
	let name = $state('');
	let display = $state('');
	let description = $state('');
	let createError = $state<string | null>(null);
	let busy = $state(false);

	const when = (iso: string) => new Date(iso).toLocaleDateString(locale.current);

	onMount(async () => {
		try {
			agents = await agentsApi.list();
		} catch (err) {
			error = (err as AgentError).message;
		} finally {
			loading = false;
		}
	});

	async function create() {
		busy = true;
		createError = null;
		try {
			const agent = await agentsApi.create({ name: name.trim(), display: display.trim() || undefined, description });
			await goto(`${base}/agents/${agent.id}`);
		} catch (err) {
			createError = (err as AgentError).message;
		} finally {
			busy = false;
		}
	}
</script>

<div class="w-full space-y-6">
	<header class="flex flex-wrap items-start justify-between gap-3">
		<div>
			<h1 class="text-2xl font-bold">{t('agents-heading')}</h1>
			<p class="mt-2 max-w-3xl text-sm text-base-content/60">{t('agents-intro')}</p>
		</div>
		<button class="btn btn-primary btn-sm" type="button" onclick={() => dialog?.showModal()}>{t('agents-create')}</button>
	</header>

	{#if error}<div class="alert alert-error"><span>{error}</span></div>{/if}

	{#if loading}
		<div class="skeleton h-28 w-full"></div>
	{:else if !agents.length && !error}
		<div class="card card-border"><div class="card-body text-sm text-base-content/60">{t('agents-list-empty')}</div></div>
	{:else}
		<div class="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
			{#each agents as agent (agent.id)}
				<a class="card card-border bg-base-100 transition hover:border-primary" href="{base}/agents/{agent.id}">
					<div class="card-body gap-2 p-4">
						<div class="flex items-start justify-between gap-2">
							<h2 class="card-title text-base">{agent.display || agent.name}</h2>
							{#if agent.live_version === null}
								<span class="badge badge-ghost badge-sm">{t('agents-never-published')}</span>
							{:else}
								<span class="badge badge-success badge-sm">{t('agents-live-badge', { version: agent.live_version })}</span>
							{/if}
						</div>
						<p class="font-mono text-xs text-base-content/60">{agent.name}</p>
						{#if agent.description}<p class="line-clamp-2 text-sm text-base-content/70">{agent.description}</p>{/if}
						<div class="mt-1 flex items-center gap-2 text-xs text-base-content/50">
							{#if agent.access === 'read'}<span class="badge badge-outline badge-xs">{t('agents-read-only')}</span>{/if}
							<span>{t('agents-updated', { date: when(agent.updated_at) })}</span>
							{#if agent.disabled_at}<span class="badge badge-error badge-xs">{t('agents-disabled')}</span>{/if}
						</div>
					</div>
				</a>
			{/each}
		</div>
	{/if}
</div>

<dialog class="modal" bind:this={dialog}>
	<form class="modal-box space-y-3" onsubmit={(e) => { e.preventDefault(); void create(); }}>
		<h3 class="text-lg font-bold">{t('agents-create')}</h3>
		{#if createError}<div class="alert alert-error text-sm" role="alert"><span>{createError}</span></div>{/if}
		<label class="flex flex-col gap-1">
			<span class="label-text">{t('agents-create-name')}</span>
			<input class="input w-full font-mono" bind:value={name} required maxlength="48" placeholder="support-website" />
			<span class="text-xs text-base-content/60">{t('agents-create-name-hint')}</span>
		</label>
		<label class="flex flex-col gap-1">
			<span class="label-text">{t('agents-create-display')}</span>
			<input class="input w-full" bind:value={display} placeholder="croit Support" />
		</label>
		<label class="flex flex-col gap-1">
			<span class="label-text">{t('agents-create-description')}</span>
			<textarea class="textarea w-full" bind:value={description}></textarea>
		</label>
		<div class="modal-action">
			<button class="btn" type="button" onclick={() => dialog?.close()}>{t('admin-cancel')}</button>
			<button class="btn btn-primary" type="submit" disabled={busy || !name.trim()}>{t('agents-create-submit')}</button>
		</div>
	</form>
	<form method="dialog" class="modal-backdrop"><button aria-label={t('admin-cancel')}>close</button></form>
</dialog>
