<script lang="ts">
	import { page } from '$app/state';
	import { t } from '#lib/i18n.svelte.js';
	import { me } from '#lib/session.svelte.js';
	import { featureEnabled } from '#lib/features.js';
	import { selectedSectionTab, type SectionTab } from '#lib/section-tabs.js';

	let { tabs, label }: { tabs: SectionTab[]; label: string } = $props();
	let selected = $derived(selectedSectionTab(page.url.pathname, tabs));
</script>

<nav class="tabs tabs-border w-full overflow-x-auto" aria-label={t(label)}>
	{#each tabs as tab (tab.path)}
		{#if !tab.feature || featureEnabled(me.value?.features, tab.feature)}
			<a class:tab-active={selected?.path === tab.path} class="tab whitespace-nowrap" href={tab.path} aria-current={selected?.path === tab.path ? 'page' : undefined}>{t(tab.label)}</a>
		{/if}
	{/each}
</nav>
