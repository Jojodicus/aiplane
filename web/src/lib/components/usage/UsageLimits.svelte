<script lang="ts">
	import { t } from '#lib/i18n.svelte.js';
	import LimitBars from './LimitBars.svelte';
	import type { UsageLimit } from '#lib/usage-types.js';

	let { limits, currency, timezone }: { limits: UsageLimit[]; currency: string; timezone: string } = $props();
</script>

<section class="card">
	<div class="card-body gap-3 p-4">
		<h2 class="card-title text-base">{t('usage-limits-heading')}</h2>
		<LimitBars {limits} {currency} {timezone} />
	</div>
</section>
