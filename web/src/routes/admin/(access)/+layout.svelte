<script lang="ts">
	import SectionTabs from '$lib/components/SectionTabs.svelte';
	import { sectionTabs } from '$lib/section-tabs';
	import { t } from '$lib/i18n.svelte';

	let { children } = $props<{ children: import('svelte').Snippet }>();
</script>

<div class="flex w-full flex-col gap-4">
	<h1 class="text-2xl font-bold">{t('admin-access-heading')}</h1>
	<SectionTabs tabs={sectionTabs.adminAccess} label="admin-access-heading" />
	{@render children()}
</div>
