<script lang="ts">
	import type { Snippet } from 'svelte';
	import { statusPillClass, type Tone } from '$lib/ui-variants';

	let { tone = 'neutral', size = 'md', children }: { tone?: Tone; size?: 'sm' | 'md'; children: Snippet } = $props();
</script>

<span class="{statusPillClass(tone)} {size === 'sm' ? 'badge-sm' : ''}">{@render children()}</span>
