<script lang="ts">
	import type { Spec } from '$lib/agents';
	import type { StepKey } from '$lib/agent-setup';
	import AbilitiesStep from './AbilitiesStep.svelte';
	import BasicsStep from './BasicsStep.svelte';
	import IdentityStep from './IdentityStep.svelte';
	import ReviewStep from './ReviewStep.svelte';
	import RoutesStep from './RoutesStep.svelte';
	import ScopeStep from './ScopeStep.svelte';
	import SiteStep from './SiteStep.svelte';
	import SlotsStep from './SlotsStep.svelte';
	import StartStep from './StartStep.svelte';

	/**
	 * One step of the setup, whichever host shows it: the assistant page binds
	 * the agent's buffer, the overview's modal binds a copy it applies on
	 * Apply. The step components themselves never know which.
	 */
	let { step, spec = $bindable(), onfix }: { step: StepKey; spec: Spec; onfix: (step: StepKey | null) => void } = $props();
</script>

{#if step === 'start'}
	<StartStep bind:spec />
{:else if step === 'basics'}
	<BasicsStep bind:spec />
{:else if step === 'scope'}
	<ScopeStep bind:spec />
{:else if step === 'abilities'}
	<AbilitiesStep bind:spec />
{:else if step === 'slots'}
	<SlotsStep bind:spec />
{:else if step === 'identity'}
	<IdentityStep bind:spec />
{:else if step === 'routes'}
	<RoutesStep bind:spec />
{:else if step === 'site'}
	<SiteStep bind:spec />
{:else}
	<ReviewStep bind:spec {onfix} />
{/if}
