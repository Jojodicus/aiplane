<script lang="ts">
	import { t } from '$lib/i18n.svelte';
</script>

<span class="inline-flex items-center gap-3">
	<img class="h-5 w-auto dark:hidden" src="/croit-logo-black.svg" alt="" aria-hidden="true" />
	<img class="hidden h-5 w-auto dark:block" src="/croit-logo-white.svg" alt="" aria-hidden="true" />
	<span class="border-l border-base-300 pl-3 text-sm font-semibold tracking-wide">{t('nav-brand')}</span>
</span>
