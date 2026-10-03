<script lang="ts">
	import { onDestroy, onMount, tick } from 'svelte';
	import { base } from '$app/paths';
	import { api } from '$lib/api';
	import { agentsApi, type AgentError } from '$lib/agents';
	import { canSend, conversationTitle, setupPath, toolLabel, undoTarget, type Phase } from '$lib/architect';
	import { createConversationController, type ConversationController } from '$lib/chat.svelte';
	import type { ToolCall } from '$lib/chat-protocol';
	import { t } from '$lib/i18n.svelte';
	import DictationButton from '$lib/components/chat/DictationButton.svelte';
	import Markdown from '$lib/components/chat/Markdown.svelte';
	import ToolCalls from '$lib/components/chat/ToolCalls.svelte';

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
	let text = $state('');
	let model = '';
	let controller = $state<ConversationController | null>(null);
	let transcriptionModel = $state('');
	let undone = $state<Record<string, boolean>>({});
	let scroller = $state<HTMLDivElement | null>(null);

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

	async function send() {
		if (!controller || !canSend(phase, text, controller.state.liveTurnId)) return;
		const message = text.trim();
		phase = 'sending';
		error = null;
		try {
			await api.sendChatMessage(controller.id, { model, message });
			text = '';
			controller.attach();
		} catch (err) {
			error = (err as Error).message;
		} finally {
			phase = 'ready';
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

	function keydown(event: KeyboardEvent) {
		if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
			event.preventDefault();
			void send();
		}
	}

	$effect(() => {
		void turns.length;
		void turns.at(-1)?.turn.content;
		void tick().then(() => scroller?.scrollTo({ top: scroller.scrollHeight }));
	});

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

	<div bind:this={scroller} class="min-h-0 flex-1 overflow-y-auto rounded-box border border-base-300 bg-base-100 p-3" aria-live="polite">
		{#if phase === 'starting'}
			<p class="flex items-center gap-2 text-sm text-base-content/60"><span class="loading loading-spinner loading-sm"></span>{t('architect-starting')}</p>
		{:else if !turns.length}
			<p class="m-0 text-sm text-base-content/60">{t('architect-empty')}</p>
		{/if}
		{#each turns as live (live.turn.id)}
			{#if live.turn.role === 'user'}
				<div class="chat chat-end">
					<div class="chat-bubble border border-primary/20 bg-primary/10 text-base-content whitespace-pre-wrap">{live.turn.user_content}</div>
				</div>
			{:else}
				<div class="chat chat-start">
					<div class="chat-bubble flex w-full max-w-full flex-col gap-2 border border-base-300 bg-base-200 text-base-content">
						{#if live.tool_calls.length}
							<ToolCalls calls={live.tool_calls} label={toolLabel(t)} />
							{@const path = agentId ? null : live.tool_calls.map(setupPath).findLast((p) => p !== null)}
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
								{#if path}<a class="btn btn-xs btn-ghost" href="{base}{path}">{t('architect-open-setup')}</a>{/if}
							</div>
						{/if}
						{#if live.turn.content}<Markdown content={live.turn.content} />{/if}
						{#if live.turn.status === 'in_progress' && !live.turn.content}
							<span class="flex items-center gap-2 text-sm text-base-content/60"><span class="loading loading-dots loading-sm"></span>{t('architect-working')}</span>
						{/if}
						{#if live.turn.error_message}<p class="m-0 text-sm text-error">{live.turn.error_message}</p>{/if}
					</div>
				</div>
			{/if}
		{/each}
	</div>

	<form class="flex items-end gap-2" onsubmit={(e) => { e.preventDefault(); void send(); }}>
		<textarea
			class="textarea min-h-11 flex-1 resize-none"
			rows="2"
			bind:value={text}
			onkeydown={keydown}
			placeholder={t('architect-placeholder')}
			aria-label={t('architect-message-label')}
			disabled={phase === 'starting' || phase === 'failed'}
		></textarea>
		{#if transcriptionModel}
			<DictationButton
				model={transcriptionModel}
				ontranscript={(heard) => (text = text.trim() ? `${text.trimEnd()} ${heard}` : heard)}
				onerror={(message) => (notice = message)}
			/>
		{/if}
		<button class="btn btn-primary" type="submit" disabled={!canSend(phase, text, controller?.state.liveTurnId ?? null)}>{t('architect-send')}</button>
	</form>
</div>
