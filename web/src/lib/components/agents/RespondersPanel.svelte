<script lang="ts">
	import { onMount } from 'svelte';
	import { agentsApi, type AgentError, type Responder } from '$lib/agents';
	import { t } from '$lib/i18n.svelte';

	/**
	 * Who answers the agent's approvals and handoffs in the inbox without a
	 * share. A responder sees the pending item and its minimal context, never
	 * the spec or the conversations, so unlike a share it needs no
	 * agent-management permission.
	 */
	let { agentId, writable }: { agentId: string; writable: boolean } = $props();

	let responders = $state<Responder[]>([]);
	let kind = $state<Responder['subject_kind']>('group');
	let subject = $state('');
	let error = $state<string | null>(null);
	let busy = $state(false);

	async function reload() {
		responders = await agentsApi.responders(agentId);
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
			await agentsApi.addResponder(agentId, { subject_kind: kind, subject_id: subject.trim() });
			subject = '';
		});
</script>

<section class="card card-border bg-base-100">
	<div class="card-body gap-4 p-4">
		<h2 class="card-title text-base">{t('agents-responders-heading')}</h2>
		<p class="text-sm text-base-content/70">{t('agents-responders-intro')}</p>
		{#if error}<div class="alert alert-error text-sm" role="alert"><span>{error}</span></div>{/if}

		<ul class="flex flex-col divide-y divide-base-300">
			{#each responders as r (r.subject_kind + r.subject_id)}
				<li class="flex flex-wrap items-center gap-2 py-2">
					<span class="badge badge-outline">{t(`agents-share-kind-${r.subject_kind}`)}</span>
					<span class="font-mono text-sm break-all">{r.subject_id}</span>
					{#if writable}
						<button class="btn btn-ghost btn-xs ml-auto" type="button" disabled={busy} onclick={() => run(() => agentsApi.removeResponder(agentId, r))}>{t('agents-responders-remove')}</button>
					{/if}
				</li>
			{:else}
				<li class="py-2 text-sm text-base-content/60">{t('agents-responders-empty')}</li>
			{/each}
		</ul>

		{#if writable}
			<form class="flex flex-wrap items-end gap-3" onsubmit={(e) => { e.preventDefault(); void add(); }}>
				<label class="flex flex-col gap-1">
					<span class="label-text">{t('agents-share-subject-kind')}</span>
					<select class="select select-sm w-32" bind:value={kind}>
						<option value="user">{t('agents-share-kind-user')}</option>
						<option value="group">{t('agents-share-kind-group')}</option>
					</select>
				</label>
				<label class="flex flex-col gap-1">
					<span class="label-text">{kind === 'user' ? t('agents-share-user-id') : t('agents-share-group-name')}</span>
					<input class="input input-sm w-64 max-w-full font-mono" bind:value={subject} required />
				</label>
				<button class="btn btn-primary btn-sm" type="submit" disabled={busy || !subject.trim()}>{t('agents-responders-add')}</button>
			</form>
		{/if}
	</div>
</section>
