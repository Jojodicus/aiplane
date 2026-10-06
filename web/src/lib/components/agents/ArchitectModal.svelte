<script lang="ts">
	import Modal from '#lib/components/ui/Modal.svelte';
	import { t } from '#lib/i18n.svelte.js';
	import ArchitectChat from './ArchitectChat.svelte';

	/**
	 * The agent architect in a centred window (never a side drawer). The
	 * conversation inside mounts while the window is open, so closing it ends
	 * the stream and reopening it picks the conversation up again.
	 */
	let {
		open = $bindable(false),
		agentId = null,
		agentName = null,
		onchanged = null
	}: {
		open?: boolean;
		agentId?: string | null;
		agentName?: string | null;
		onchanged?: (() => void) | null;
	} = $props();
</script>

<Modal bind:open title={t('architect-title')} size="lg">
	<ArchitectChat {agentId} {agentName} {onchanged} />
</Modal>
