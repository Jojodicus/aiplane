<script lang="ts">
	import type { Snippet } from 'svelte';
	import Modal from '#lib/components/ui/Modal.svelte';

	/**
	 * The one editor dialog the admin rows share: `ui/Modal` with a footer.
	 *
	 * Replaces the inline `<details>` editors: a row's Edit button opens this
	 * instead of pushing the row open and shoving the rest of the list down
	 * the page. Small forms only — anything multi-section gets its own route.
	 *
	 * `open` is bindable so the caller owns the state (and can reset its draft
	 * when the dialog opens); `Modal` keeps the real `<dialog>` in step.
	 *
	 * Three footer shapes, because the editors are three shapes:
	 *   `save`  — Cancel + Save, for a draft the caller commits (`onsave`).
	 *   `close` — one Close button, for panels whose controls already apply
	 *             on change (a token's capability toggles).
	 *   `none`  — the child is a whole `<form>` with its own actions (the
	 *             upstream pool and backend editors).
	 *
	 * Nothing inside renders while closed (Modal's guarantee) — these sit
	 * inside list rows, and it keeps a closed dialog's title out of the
	 * document, so a row's "Edit pool" button is the only thing by that name.
	 */
	let {
		open = $bindable(false),
		title,
		description = null,
		wide = false,
		saving = false,
		footer = 'save',
		savelabel = null,
		cancellabel,
		onsave = null,
		children
	}: {
		open?: boolean;
		title: string;
		description?: string | null;
		wide?: boolean;
		saving?: boolean;
		footer?: 'save' | 'close' | 'none';
		savelabel?: string | null;
		cancellabel: string;
		onsave?: (() => void | Promise<void>) | null;
		children: Snippet;
	} = $props();
</script>

{#snippet actions()}
	{#if footer === 'save' && onsave}
		<button type="button" class="btn btn-ghost btn-sm" onclick={() => (open = false)}>{cancellabel}</button>
		<button type="button" class="btn btn-primary btn-sm" disabled={saving} onclick={onsave}>{savelabel}</button>
	{:else if footer === 'close'}
		<button type="button" class="btn btn-sm" onclick={() => (open = false)}>{cancellabel}</button>
	{/if}
{/snippet}

<Modal bind:open {title} {description} size={wide ? 'lg' : 'md'} footer={footer === 'none' ? null : actions}>
	{@render children()}
</Modal>
