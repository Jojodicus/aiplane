<script lang="ts">
	import { agentsApi, type AgentError, type Share } from '$lib/agents';
	import { t } from '$lib/i18n.svelte';

	/**
	 * Who else may work on this agent. A share takes effect only for a holder
	 * of the agent-management permission (it shows the spec and the visitors'
	 * conversations), and the last `write` share cannot be removed. Both
	 * refusals come back from the server in words and are shown as they are.
	 */
	let { agentId, shares, writable, onchanged }: {
		agentId: string;
		shares: Share[];
		writable: boolean;
		onchanged: () => void | Promise<void>;
	} = $props();

	let kind = $state<Share['subject_kind']>('user');
	let subject = $state('');
	let access = $state<Share['access']>('read');
	let error = $state<string | null>(null);
	let busy = $state(false);

	async function run(action: () => Promise<unknown>) {
		busy = true;
		error = null;
		try {
			await action();
			await onchanged();
		} catch (err) {
			error = (err as AgentError).message;
		} finally {
			busy = false;
		}
	}
	const add = () =>
		run(async () => {
			await agentsApi.share(agentId, { subject_kind: kind, subject_id: subject.trim(), access });
			subject = '';
		});
</script>

<div class="space-y-4">
	<p class="text-sm text-base-content/70">{t('agents-share-intro')}</p>
	{#if error}<div class="alert alert-error text-sm" role="alert"><span>{error}</span></div>{/if}

	<div class="overflow-x-auto">
		<table class="table table-sm">
			<thead>
				<tr><th>{t('agents-share-subject')}</th><th>{t('agents-share-access')}</th><th></th></tr>
			</thead>
			<tbody>
				{#each shares as share (share.subject_kind + share.subject_id)}
					<tr>
						<td>
							<span class="badge badge-outline mr-2">{t(`agents-share-kind-${share.subject_kind}`)}</span>
							<span class="font-mono text-sm">{share.subject_id}</span>
						</td>
						<td>
							{#if writable}
								<select class="select select-xs w-28" value={share.access} disabled={busy} onchange={(e) => run(() => agentsApi.share(agentId, { ...share, access: e.currentTarget.value as Share['access'] }))} aria-label={t('agents-share-access')}>
									<option value="read">{t('agents-share-read')}</option>
									<option value="write">{t('agents-share-write')}</option>
								</select>
							{:else}
								{t(`agents-share-${share.access}`)}
							{/if}
						</td>
						<td class="text-right">
							{#if writable}
								<button class="btn btn-ghost btn-xs" type="button" disabled={busy} onclick={() => run(() => agentsApi.revokeShare(agentId, share))}>{t('agents-share-revoke')}</button>
							{/if}
						</td>
					</tr>
				{:else}
					<tr><td colspan="3" class="text-sm text-base-content/60">{t('agents-share-empty')}</td></tr>
				{/each}
			</tbody>
		</table>
	</div>

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
				<input class="input input-sm w-64 font-mono" bind:value={subject} required />
			</label>
			<label class="flex flex-col gap-1">
				<span class="label-text">{t('agents-share-access')}</span>
				<select class="select select-sm w-28" bind:value={access}>
					<option value="read">{t('agents-share-read')}</option>
					<option value="write">{t('agents-share-write')}</option>
				</select>
			</label>
			<button class="btn btn-primary btn-sm" type="submit" disabled={busy || !subject.trim()}>{t('agents-share-add')}</button>
		</form>
	{/if}
</div>
