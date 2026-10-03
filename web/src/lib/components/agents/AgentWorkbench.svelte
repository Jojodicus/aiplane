<script lang="ts">
	import { onMount } from 'svelte';
	import { base } from '$app/paths';
	import { goto, replaceState } from '$app/navigation';
	import { page } from '$app/state';
	import {
		agentsApi,
		cleanSpec,
		ensureShape,
		type AgentDetail,
		type AgentError,
		type AgentResources,
		type AgentSummary,
		type AgentVersion,
		type Granted,
		type Spec,
		type SpecIssue,
		type TestDebug
	} from '$lib/agents';
	import { t } from '$lib/i18n.svelte';
	import AgentEditor from './AgentEditor.svelte';
	import AgentCanvas from './AgentCanvas.svelte';
	import ActivityPanel from './ActivityPanel.svelte';
	import AnalyticsPanel from './AnalyticsPanel.svelte';
	import TestsPanel from './TestsPanel.svelte';
	import GrantsPanel from './GrantsPanel.svelte';
	import SharingPanel from './SharingPanel.svelte';
	import RespondersPanel from './RespondersPanel.svelte';
	import ChannelsPanel from './ChannelsPanel.svelte';
	import EmbedKeysPanel from './EmbedKeysPanel.svelte';
	import SpecJsonEditor from './SpecJsonEditor.svelte';
	import TestChat from './TestChat.svelte';
	import VersionsPanel from './VersionsPanel.svelte';

	/**
	 * One agent: the builder, its JSON, grants, the test chat, versions and
	 * sharing. The spec being edited is a buffer; **Save draft** sends it, and
	 * the server's validation answer is kept as `issues` and shown beside the
	 * fields it is about. The test chat and Publish both act on what is saved,
	 * so both save first when the buffer is ahead.
	 */
	let { id }: { id: string } = $props();

	const TABS = ['edit', 'canvas', 'json', 'grants', 'test', 'tests', 'versions', 'analytics', 'activity', 'sharing'] as const;
	type Tab = (typeof TABS)[number];
	const asTab = (v: string | null): Tab => (TABS.find((x) => x === v) ?? 'edit');

	let detail = $state<AgentDetail | null>(null);
	let versions = $state<AgentVersion[]>([]);
	let resources = $state<AgentResources | null>(null);
	let agents = $state<AgentSummary[]>([]);
	let spec = $state<Spec>(ensureShape({}));
	let savedJson = $state('');
	let formKey = $state(0);
	let issues = $state<SpecIssue[]>([]);
	let loading = $state(true);
	let loadError = $state<string | null>(null);
	let error = $state<string | null>(null);
	let notice = $state<string | null>(null);
	let busy = $state(false);
	let lastDebug = $state<TestDebug | null>(null);
	let tab = $state<Tab>(asTab(page.url.searchParams.get('tab')));

	const writable = $derived(detail?.access === 'write');
	const dirty = $derived(JSON.stringify(cleanSpec(spec)) !== savedJson);
	const granted = $derived.by((): Granted => {
		const grants = detail?.grants ?? [];
		const toolName = (tid: string) => resources?.tools.find((x) => x.id === tid)?.name ?? tid;
		const connectorTools = grants
			.filter((g) => g.kind === 'connector')
			.flatMap((g) => resources?.connectors.find((c) => c.key === g.ref)?.tools ?? []);
		return {
			pools: grants.filter((g) => g.kind === 'pool').map((g) => g.ref),
			tools: [...grants.filter((g) => g.kind === 'tool').map((g) => g.ref), ...connectorTools].map((tid) => ({ id: tid, name: toolName(tid) })),
			skills: grants.filter((g) => g.kind === 'skill').map((g) => g.ref)
		};
	});

	function adopt(next: AgentDetail) {
		detail = next;
		const shaped = ensureShape(next.draft_spec);
		spec = shaped;
		savedJson = JSON.stringify(cleanSpec(shaped));
		formKey++;
	}

	async function refresh(keepBuffer = false) {
		const [next, vs] = await Promise.all([agentsApi.get(id), agentsApi.versions(id)]);
		versions = vs.versions;
		if (keepBuffer) detail = next;
		else adopt(next);
	}

	onMount(async () => {
		try {
			await refresh();
			const [r, a] = await Promise.all([agentsApi.resources(), agentsApi.list()]);
			resources = r;
			agents = a.filter((x) => x.id !== id);
		} catch (err) {
			loadError = (err as AgentError).message;
		} finally {
			loading = false;
		}
	});

	function selectTab(next: Tab) {
		tab = next;
		const url = new URL(page.url);
		url.searchParams.set('tab', next);
		replaceState(url, {});
	}

	function fail(err: unknown) {
		const e = err as AgentError;
		error = e.message;
		issues = e.issues ?? [];
	}

	async function save(): Promise<boolean> {
		busy = true;
		error = null;
		notice = null;
		try {
			const clean = cleanSpec(spec);
			await agentsApi.saveDraft(id, clean);
			savedJson = JSON.stringify(clean);
			issues = [];
			await refresh(true);
			notice = t('agents-saved');
			return true;
		} catch (err) {
			fail(err);
			return false;
		} finally {
			busy = false;
		}
	}

	/** A rollback (or roll forward) publishes nothing, so it replaces whatever the last save or publish said. */
	async function madeLive(version: number) {
		error = null;
		notice = t('agents-live-is', { version });
		await refresh(true);
	}

	async function publish() {
		if (dirty && !(await save())) return;
		busy = true;
		error = null;
		notice = null;
		try {
			const result = await agentsApi.publish(id);
			issues = [];
			await refresh(true);
			notice = t('agents-published', { version: result.version });
		} catch (err) {
			fail(err);
			if (issues.length) selectTab('edit');
			throw err;
		} finally {
			busy = false;
		}
	}

	async function remove() {
		if (!detail || !confirm(t('agents-delete-confirm', { name: detail.name }))) return;
		try {
			await agentsApi.remove(id);
			await goto(`${base}/agents`);
		} catch (err) {
			fail(err);
		}
	}

	function applyJson(parsed: Spec) {
		spec = ensureShape(parsed);
		formKey++;
	}
</script>

<div class="w-full space-y-4">
	<a class="link link-hover text-sm text-base-content/60" href="{base}/agents">← {t('agents-back')}</a>

	{#if loadError}
		<div class="alert alert-error"><span>{loadError}</span></div>
	{:else if loading || !detail}
		<div class="skeleton h-96 w-full"></div>
	{:else}
		<header class="flex flex-wrap items-center gap-3">
			<div class="min-w-0">
				<h1 class="truncate text-2xl font-bold">{detail.display || detail.name}</h1>
				<p class="font-mono text-xs text-base-content/60">{detail.name}</p>
			</div>
			{#if detail.live_version === null}
				<span class="badge badge-ghost">{t('agents-never-published')}</span>
			{:else}
				<span class="badge badge-success">{t('agents-live-badge', { version: detail.live_version })}</span>
			{/if}
			{#if dirty}<span class="badge badge-warning">{t('agents-unsaved')}</span>{/if}
			{#if !writable}<span class="badge badge-outline">{t('agents-read-only')}</span>{/if}
			{#if writable}
				<div class="ml-auto flex flex-wrap gap-2">
					<button class="btn btn-primary btn-sm" type="button" disabled={busy || !dirty} onclick={() => void save()}>{t('agents-save')}</button>
					<button class="btn btn-sm" type="button" disabled={busy} onclick={() => void publish().catch(() => {})}>{t('agents-publish-action')}</button>
					<button class="btn btn-ghost btn-sm text-error" type="button" onclick={() => void remove()}>{t('agents-delete')}</button>
				</div>
			{/if}
		</header>

		{#if error}
			<div class="alert alert-error text-sm" role="alert">
				<div>
					<p>{error}</p>
					{#if issues.length > 1}
						<ul class="mt-1 list-inside list-disc">
							{#each issues as issue (issue.path + issue.message)}
								<li><span class="font-mono">{issue.path || t('agents-issue-root')}</span>: {issue.message}</li>
							{/each}
						</ul>
					{/if}
				</div>
			</div>
		{/if}
		{#if notice}<div class="alert alert-success text-sm"><span>{notice}</span></div>{/if}

		<div role="tablist" class="tabs tabs-border w-full overflow-x-auto">
			{#each TABS as name (name)}
				<button role="tab" type="button" class="tab whitespace-nowrap" class:tab-active={tab === name} aria-selected={tab === name} onclick={() => selectTab(name)}>
					{t(`agents-tab-${name}`)}
				</button>
			{/each}
		</div>

		<div class="pt-2">
			{#if tab === 'edit'}
				{#key formKey}
					<fieldset disabled={!writable} class="min-w-0">
						<AgentEditor bind:spec {issues} {granted} {agents} ongrants={() => selectTab('grants')} />
					</fieldset>
				{/key}
			{:else if tab === 'canvas'}
				<fieldset disabled={!writable} class="min-w-0">
					<AgentCanvas bind:spec {issues} {granted} {agents} {lastDebug} ongrants={() => selectTab('grants')} />
				</fieldset>
			{:else if tab === 'json'}
				{#key formKey}
					<SpecJsonEditor {spec} {issues} onapply={applyJson} />
				{/key}
			{:else if tab === 'grants'}
				<GrantsPanel agentId={id} grants={detail.grants} {resources} {writable} onchanged={() => refresh(true)} />
			{:else if tab === 'test'}
				<TestChat agentId={id} {dirty} onturn={(debug) => (lastDebug = debug)} onsave={async () => void (await save())} />
			{:else if tab === 'tests'}
				<TestsPanel agentId={id} {versions} liveVersion={detail.live_version} {dirty} {writable} onsave={async () => void (await save())} />
			{:else if tab === 'versions'}
				<VersionsPanel
					agentId={id}
					liveVersion={detail.live_version}
					{versions}
					publishIssues={detail.publish_issues}
					{dirty}
					{writable}
					onpublish={publish}
					onlive={madeLive}
				/>
			{:else if tab === 'analytics'}
				<AnalyticsPanel agentId={id} {versions} />
			{:else if tab === 'activity'}
				<ActivityPanel agentId={id} />
			{:else}
				<div class="space-y-6">
					<SharingPanel agentId={id} shares={detail.shares} {writable} onchanged={() => refresh(true)} />
					<RespondersPanel agentId={id} {writable} />
					<ChannelsPanel agentId={id} {writable} />
					<EmbedKeysPanel agentId={id} {writable} />
				</div>
			{/if}
		</div>
	{/if}
</div>
