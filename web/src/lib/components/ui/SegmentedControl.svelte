<script lang="ts" generics="V extends string">
	import { segmentClass } from '$lib/ui-variants';

	/** A small closed choice (a handful of options) shown all at once, as a daisyUI `join`. */
	let {
		options,
		value = $bindable(),
		label,
		size = 'md',
		disabled = false
	}: {
		options: readonly { value: V; label: string }[];
		value: V;
		label: string;
		size?: 'sm' | 'md';
		disabled?: boolean;
	} = $props();
</script>

<div class="join" role="radiogroup" aria-label={label}>
	{#each options as option (option.value)}
		<button
			type="button"
			role="radio"
			aria-checked={value === option.value}
			class="{segmentClass(value === option.value)} {size === 'sm' ? 'btn-sm' : ''}"
			{disabled}
			onclick={() => (value = option.value)}>{option.label}</button
		>
	{/each}
</div>
