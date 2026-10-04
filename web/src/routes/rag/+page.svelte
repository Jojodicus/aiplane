<script lang="ts">
	import { onMount } from 'svelte';
	import { base } from '$app/paths';
	import { page } from '$app/state';
	import { adminJson } from '$lib/admin-client';
	import CollectionCard from '$lib/components/rag/CollectionCard.svelte';
	import { t } from '$lib/i18n.svelte';
	import { listNotice, type RagCollection } from '$lib/rag';

	let collections = $state<RagCollection[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let notice = $state<string | null>(null);

	// The create/edit form lives on its own route and reports back through the
	// URL, so its confirmation shows up on the list the operator returns to.
	$effect(() => {
		const shown = listNotice(page.url.searchParams);
		if (shown) notice = t(shown.key, shown.args);
	});

	async function refreshCollections() {
		collections = (await adminJson<{ data: RagCollection[] }>('/api/v0/rag/collections')).data ?? [];
	}

	async function load() {
		loading = true;
		try {
			// Providers, profiles and embedding models are the editor's inputs;
			// the list itself only needs the collections.
			collections = (await adminJson<{ data: RagCollection[] }>('/api/v0/rag/collections')).data ?? [];
			error = null;
		} catch (caught) {
			error = String(caught);
		} finally {
			loading = false;
		}
	}

	onMount(load);
</script>

<div class="w-full space-y-6">
	<header>
		<h1 class="text-2xl font-bold">{t('rag-heading')}</h1>
		<p class="mt-2 text-sm text-base-content/60">{t('rag-description-prefix')} <code>rag_search</code> {t('rag-description-suffix')}</p>
		<a class="link mt-2 inline-block text-sm" href="/rag/profiles">{t('rag-profile-link')}</a>
	</header>

	{#if error}<div class="alert alert-error"><span>{error}</span></div>{/if}
	{#if notice}<div class="alert alert-info"><span>{notice}</span></div>{/if}

	<section class="space-y-3">
		<div class="flex flex-wrap items-center justify-between gap-3">
			<h2 class="text-xl font-semibold">{t('rag-collections-heading')}</h2>
			<a class="btn btn-primary btn-sm" href="{base}/rag/new">{t('rag-create-heading')}</a>
		</div>
		{#if loading}
			<div class="skeleton h-28 w-full"></div>
		{:else if collections.length === 0}
			<div class="card card-border"><div class="card-body text-sm text-base-content/60">{t('rag-empty-list')}</div></div>
		{:else}
			{#each collections as collection (collection.id)}
				<CollectionCard {collection} onchanged={refreshCollections} onnotice={(message) => (notice = message)} />
			{/each}
		{/if}
	</section>
</div>
