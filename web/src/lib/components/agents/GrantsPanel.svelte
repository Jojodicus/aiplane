<script lang="ts">
	import { GRANT_KINDS, agentsApi, grantOptions, type AgentResources, type AgentError, type Grant, type GrantKind } from '$lib/agents';
	import { t } from '$lib/i18n.svelte';

	/**
	 * What the agent's principal may use. A new agent holds nothing, and a
	 * manager can grant only what they hold themselves, checked when the grant
	 * is made. The server's refusal is shown as it is: it says which grant is
	 * missing and who can make it.
	 */
	let { agentId, grants, resources, writable, onchanged }: {
		agentId: string;
		grants: Grant[];
		resources: AgentResources | null;
		writable: boolean;
		onchanged: () => void | Promise<void>;
	} = $props();

	let kind = $state<GrantKind>('model');
	let ref = $state('');
	let error = $state<string | null>(null);
	let busy = $state(false);

	const options = $derived.by((): { value: string; label: string }[] => {
		if (!resources) return [];
		switch (kind) {
			case 'model':
				return [...new Set(Object.values(resources.models ?? {}).flatMap((list) => list.map((m) => m.id)))].map((v) => ({ value: v, label: v }));
			default:
				return grantOptions(resources, kind);
		}
	});

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
			await agentsApi.grant(agentId, kind, ref.trim());
			ref = '';
		});
</script>

<div class="space-y-4">
	<p class="text-sm text-base-content/70">{t('agents-grants-intro')}</p>
	{#if error}<div class="alert alert-error text-sm" role="alert"><span>{error}</span></div>{/if}

	<div class="overflow-x-auto">
		<table class="table table-sm">
			<thead>
				<tr><th>{t('agents-grants-kind')}</th><th>{t('agents-grants-ref')}</th><th>{t('agents-grants-by')}</th><th></th></tr>
			</thead>
			<tbody>
				{#each grants as grant (grant.kind + grant.ref)}
					<tr>
						<td><span class="badge badge-outline">{t(`agents-grant-kind-${grant.kind}`)}</span></td>
						<td class="font-mono text-sm">{grant.ref}</td>
						<td class="text-sm text-base-content/60">{grant.granted_by}</td>
						<td class="text-right">
							{#if writable}
								<button class="btn btn-ghost btn-xs" type="button" disabled={busy} onclick={() => run(() => agentsApi.revokeGrant(agentId, grant.kind, grant.ref))}>{t('agents-grants-revoke')}</button>
							{/if}
						</td>
					</tr>
				{:else}
					<tr><td colspan="4" class="text-sm text-base-content/60">{t('agents-grants-empty')}</td></tr>
				{/each}
			</tbody>
		</table>
	</div>

	{#if writable}
		<form class="flex flex-wrap items-end gap-3" onsubmit={(e) => { e.preventDefault(); void add(); }}>
			<label class="flex flex-col gap-1">
				<span class="label-text">{t('agents-grants-kind')}</span>
				<select class="select w-40" bind:value={kind} onchange={() => (ref = '')}>
					{#each GRANT_KINDS as k (k)}<option value={k}>{t(`agents-grant-kind-${k}`)}</option>{/each}
				</select>
			</label>
			<label class="flex flex-col gap-1">
				<span class="label-text">{t('agents-grants-ref')}</span>
				<input class="input w-72 font-mono" list="agent-grant-options" bind:value={ref} required />
				<datalist id="agent-grant-options">
					{#each options as option (option.value)}<option value={option.value}>{option.label}</option>{/each}
				</datalist>
			</label>
			<button class="btn btn-primary" type="submit" disabled={busy || !ref.trim()}>{t('agents-grants-add')}</button>
		</form>
		<p class="text-xs text-base-content/60">{t('agents-grants-hint')}</p>
	{/if}
</div>
