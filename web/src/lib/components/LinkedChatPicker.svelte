<script lang="ts">
	import { onMount } from 'svelte';
	import { api } from '$lib/api';
	import { t } from '$lib/i18n.svelte';
	import SearchableSelect from '$lib/components/SearchableSelect.svelte';
	import { linkedChatOptions } from '$lib/linked-chat';
	import type { ChatSession } from '$lib/chat-protocol';

	/**
	 * Which chat a reusing schedule or webhook continues in. Shared by both
	 * forms, so the choice reads and behaves the same on each.
	 */
	let { value = $bindable('') } = $props<{ value?: string }>();
	let sessions = $state<ChatSession[]>([]);
	let options = $derived(linkedChatOptions(sessions, {
		fresh: t('linked-chat-fresh'),
		untitled: t('nav-untitled-chat')
	}));

	onMount(async () => {
		try {
			sessions = (await api.listChatSessions()).sessions;
		} catch {
			// Without the list the picker still offers a fresh chat, and the
			// current link stays untouched unless the user picks that.
			sessions = [];
		}
	});
</script>

<fieldset class="fieldset w-full min-w-0">
	<legend class="fieldset-legend">{t('linked-chat-label')}</legend>
	<SearchableSelect {options} bind:value ariaLabel={t('linked-chat-label')} class="w-full" />
	<p class="label whitespace-normal">{t('linked-chat-help')}</p>
</fieldset>
