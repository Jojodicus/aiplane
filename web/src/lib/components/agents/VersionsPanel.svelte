<script lang="ts">
	import { agentsApi, type AgentError, type AgentVersion, type SpecIssue } from '$lib/agents';
	import { locale, t } from '$lib/i18n.svelte';

	/**
	 * Draft and published versions. Publishing snapshots the draft as the next
	 * version and makes it live; going back is making an older snapshot live.
	 * Grants are not versioned, so a rolled-back version meets today's grants.
	 */
	let { agentId, liveVersion, versions, publishIssues, dirty, writable, onpublish, onchanged }: {
		agentId: string;
		liveVersion: number | null;
		versions: AgentVersion[];
		publishIssues: SpecIssue[];
		dirty: boolean;
		writable: boolean;
		onpublish: () => Promise<void>;
		onchanged: () => void | Promise<void>;
	} = $props();

	let error = $state<string | null>(null);
	let busy = $state(false);

	const when = (iso: string) => new Date(iso).toLocaleString(locale.current);

	async function makeLive(version: number) {
		if (!confirm(t('agents-versions-live-confirm', { version }))) return;
		busy = true;
		error = null;
		try {
			await agentsApi.setLive(agentId, version);
			await onchanged();
		} catch (err) {
			error = (err as AgentError).message;
		} finally {
			busy = false;
		}
	}
</script>

<div class="space-y-4">
	{#if error}<div class="alert alert-error text-sm" role="alert"><span>{error}</span></div>{/if}

	<div class="card card-border">
		<div class="card-body gap-2 p-4">
			<div class="flex flex-wrap items-center gap-2">
				<span class="badge badge-warning">{t('agents-draft')}</span>
				{#if dirty}<span class="badge badge-ghost">{t('agents-unsaved')}</span>{/if}
				<span class="text-sm text-base-content/60">
					{liveVersion === null ? t('agents-never-published') : t('agents-live-is', { version: liveVersion })}
				</span>
				{#if writable}
					<button class="btn btn-primary btn-sm ml-auto" type="button" disabled={busy || publishIssues.length > 0} onclick={() => { busy = true; error = null; onpublish().catch((e: AgentError) => (error = e.message)).finally(() => (busy = false)); }}>{t('agents-publish-action')}</button>
				{/if}
			</div>
			{#if publishIssues.length}
				<div class="text-sm">
					<p class="mb-1 font-semibold text-warning">{t('agents-publish-blocked', { count: publishIssues.length })}</p>
					<ul class="list-inside list-disc text-base-content/70">
						{#each publishIssues as issue (issue.path + issue.message)}
							<li><span class="font-mono">{issue.path || t('agents-issue-root')}</span>: {issue.message}</li>
						{/each}
					</ul>
				</div>
			{/if}
		</div>
	</div>

	<div class="space-y-2">
		{#each versions as version (version.version)}
			<div class="collapse collapse-arrow border border-base-300">
				<input type="checkbox" aria-label={t('agents-version-label', { version: version.version })} />
				<div class="collapse-title flex flex-wrap items-center gap-2">
					<span class="font-semibold">{t('agents-version-label', { version: version.version })}</span>
					{#if version.version === liveVersion}<span class="badge badge-success badge-sm">{t('agents-live')}</span>{/if}
					<span class="text-xs font-normal text-base-content/60">{t('agents-version-by', { by: version.published_by, at: when(version.published_at) })}</span>
				</div>
				<div class="collapse-content space-y-2">
					{#if writable && version.version !== liveVersion}
						<button class="btn btn-sm" type="button" disabled={busy} onclick={() => makeLive(version.version)}>{t('agents-versions-make-live')}</button>
					{/if}
					<pre class="max-h-96 overflow-auto rounded-box bg-base-200 p-3 text-xs">{JSON.stringify(version.spec, null, 2)}</pre>
				</div>
			</div>
		{:else}
			<p class="text-sm text-base-content/60">{t('agents-versions-empty')}</p>
		{/each}
	</div>
</div>
