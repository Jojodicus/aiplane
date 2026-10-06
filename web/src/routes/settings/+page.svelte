<script lang="ts">
	import { onMount } from 'svelte';
	import { adminJson } from '#lib/admin-client.js';
	import TokenAccountCard from '#lib/components/tokens/TokenAccountCard.svelte';
	import type { TokenManagementDetails } from '#lib/tokens.js';

	let details = $state<TokenManagementDetails | null>(null);
	let error = $state<string | null>(null);

	onMount(async () => {
		try { details = await adminJson<TokenManagementDetails>('/api/v0/tokens/details'); }
		catch (caught) { error = String(caught); }
	});
</script>

{#if error}<div class="alert alert-error"><span>{error}</span></div>{/if}
{#if details}<TokenAccountCard account={details.account} />{/if}
