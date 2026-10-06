<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { api } from '#lib/api.js';
	import { agentsApi, type AgentError } from '#lib/agents.js';
	import { conversationTitle, setupPath, toolLabel, undoTarget, type Phase } from '#lib/architect.js';
	import { createConversationController, type ConversationController } from '#lib/chat.svelte.js';
	import type { ToolCall } from '#lib/chat-protocol.js';
	import { t } from '#lib/i18n.svelte.js';
	import DictationButton from '#lib/components/chat/DictationButton.svelte';
	import StreamedChat from '#lib/components/chat/StreamedChat.svelte';

	/**
	 * The architect's conversation, mounted while its window is open: it opens
	 * (or reopens) the person's architect chat about `agentId`, streams it
	 * like any chat, and sends what is typed or dictated. Every tool call shows
	 * with its input and output; a draft change offers Undo.
	 */
	let {
		agentId = null,
		agentName = null,
		onchanged = null
	}: {
		agentId?: string | null;
		agentName?: string | null;
		onchanged?: (() => void) | null;
	} = $props();

	let phase = $state<Phase>('starting');
	let error = $state<string | null>(null);
	let notice = $state<string | null>(null);
	let model = '';
	let controller = $state<ConversationController | null>(null);
	let transcriptionModel = $state('');
	let undone = $state<Record<string, boolean>>({});

	const turns = $derived(controller?.state.turns ?? []);
	const working = $derived(controller?.state.liveTurnId != null);

	async function start(fresh: boolean) {
		phase = 'starting';
		error = null;
		controller?.destroy();
		controller = null;
		try {
			const started = await agentsApi.startArchitect({
				agent_id: agentId ?? undefined,
				title: conversationTitle(t, agentName),
				fresh
			});
			model = started.model;
			const c = createConversationController(started.session_id);
			c.onTurnFinalized = () => onchanged?.();
			c.attach();
			controller = c;
			phase = 'ready';
		} catch (err) {
			error = (err as AgentError).message;
			phase = 'failed';
		}
	}

	async function send(message: string): Promise<boolean> {
		if (!controller) return false;
		error = null;
		try {
			await api.sendChatMessage(controller.id, { model, message });
			controller.attach();
			return true;
		} catch (err) {
			error = (err as Error).message;
			return false;
		}
	}

	async function undo(call: ToolCall) {
		const target = undoTarget(call);
		if (!target) return;
		try {
			await agentsApi.restoreDraft(target.agentId, target.revision);
			undone = { ...undone, [call.id]: true };
			onchanged?.();
		} catch (err) {
			error = (err as AgentError).message;
		}
	}

	onMount(async () => {
		void start(false);
		try {
			transcriptionModel = (await api.chatVoiceConfig()).data[0] ?? '';
		} catch {
			transcriptionModel = '';
		}
	});

	onDestroy(() => controller?.destroy());
</script>

<div class="flex h-[min(70dvh,44rem)] min-h-80 flex-col gap-3">
	<div class="flex items-center gap-2">
		<p class="m-0 flex-1 text-sm text-base-content/60">{t('architect-intro')}</p>
		<button class="btn btn-ghost btn-sm" type="button" disabled={phase === 'starting' || working} onclick={() => void start(true)}>{t('architect-new-conversation')}</button>
	</div>

	{#if error}<div class="alert alert-error text-sm" role="alert"><span>{error}</span></div>{/if}
	{#if notice}<div class="alert alert-warning text-sm" role="status"><span>{notice}</span></div>{/if}

	<StreamedChat
		{turns}
		{working}
		disabled={phase !== 'ready'}
		empty={phase === 'starting' ? '' : t('architect-empty')}
		placeholder={t('architect-placeholder')}
		sendLabel={t('architect-send')}
		toolLabel={toolLabel(t)}
		onsend={send}
	>
		{#snippet before()}
			{#if phase === 'starting'}
				<p class="m-0 flex items-center gap-2 text-sm text-base-content/60"><span class="loading loading-spinner loading-sm"></span>{t('architect-starting')}</p>
			{/if}
		{/snippet}
		{#snippet inside(live)}
			{@const path = agentId ? null : live.tool_calls.map(setupPath).findLast((p) => p !== null)}
			{#if live.tool_calls.some((call) => undoTarget(call)) || path}
				<div class="flex flex-wrap gap-2">
					{#each live.tool_calls as call (call.id)}
						{#if undoTarget(call)}
							{#if undone[call.id]}
								<span class="badge badge-outline">{t('architect-undone')}</span>
							{:else}
								<button class="btn btn-xs" type="button" onclick={() => void undo(call)}>↶ {t('architect-undo')}</button>
							{/if}
						{/if}
					{/each}
					{#if path}<a class="btn btn-xs btn-ghost" href={path}>{t('architect-open-setup')}</a>{/if}
				</div>
			{/if}
		{/snippet}
		{#snippet composer(append)}
			{#if transcriptionModel}
				<DictationButton model={transcriptionModel} ontranscript={append} onerror={(message) => (notice = message)} />
			{/if}
		{/snippet}
	</StreamedChat>
</div>
