<script lang="ts">
	import {
		agentsApi,
		suspensionLabel,
		answerField,
		testTurnLabel,
		type AgentError,
		type ResumeDecision,
		type Suspension,
		type TestDebug,
		type TestTurn
	} from '$lib/agents';
	import { t } from '$lib/i18n.svelte';
	import DebugPanel from './DebugPanel.svelte';

	/**
	 * The internal test chat: talks to the agent's saved **draft** the way a
	 * visitor would talk to the published one, and shows the manager-only
	 * debug view beside it. The agent's tools really run, as the agent's
	 * principal. A conversation is the server's `session_id`; "new
	 * conversation" simply forgets it.
	 */
	let { agentId, dirty, onsave, onturn }: {
		agentId: string;
		dirty: boolean;
		onsave: () => Promise<void>;
		onturn?: (debug: TestDebug | null) => void;
	} = $props();

	type Message = {
		role: 'visitor' | 'agent';
		text: string;
		status?: string;
		error?: string | null;
		debug?: TestDebug;
		turnId?: string;
		suspension?: Suspension | null;
	};
	let messages = $state<Message[]>([]);
	let sessionId = $state<string | null>(null);
	let draft = $state('');
	let secret = $state('');

	function agentMessage(turn: TestTurn, debug?: TestDebug): Message {
		return {
			role: 'agent',
			text: turn.answer ?? '',
			status: turn.status,
			error: turn.error,
			debug,
			turnId: turn.turn_id,
			suspension: turn.suspension
		};
	}

	/** Answer the pause of message `i`; the reply replaces it, as the same turn continued. */
	async function answer(i: number, decision: ResumeDecision) {
		const waiting = messages[i];
		if (busy || !sessionId || !waiting?.turnId || !waiting.suspension) return;
		busy = true;
		error = null;
		try {
			const turn = await agentsApi.resumeTurn(
				agentId,
				sessionId,
				waiting.turnId,
				waiting.suspension.request_id,
				decision
			);
			messages[i] = agentMessage(turn, turn.debug ?? waiting.debug);
			secret = '';
		} catch (err) {
			error = (err as AgentError).message;
		} finally {
			busy = false;
		}
	}
	let busy = $state(false);
	let error = $state<string | null>(null);
	let selected = $state<number | null>(null);

	const shown = $derived(
		selected !== null && messages[selected]?.debug
			? messages[selected].debug
			: ([...messages].reverse().find((m) => m.debug)?.debug ?? null)
	);

	$effect(() => {
		const latest = [...messages].reverse().find((m) => m.debug)?.debug;
		if (latest) onturn?.(latest);
	});

	async function send() {
		const text = draft.trim();
		if (!text || busy) return;
		busy = true;
		error = null;
		messages.push({ role: 'visitor', text });
		draft = '';
		selected = null;
		try {
			const turn = await agentsApi.testTurn(agentId, text, sessionId);
			sessionId = turn.session_id;
			messages.push(agentMessage(turn, turn.debug));
		} catch (err) {
			error = (err as AgentError).message;
		} finally {
			busy = false;
		}
	}
	function reset() {
		onturn?.(null);
		messages = [];
		sessionId = null;
		selected = null;
		error = null;
	}
</script>

<div class="space-y-3">
	<div class="flex flex-wrap items-center gap-2">
		<p class="text-sm text-base-content/70">{t('agents-test-intro')}</p>
		<button class="btn btn-ghost btn-sm ml-auto" type="button" onclick={reset} disabled={!messages.length}>{t('agents-test-new')}</button>
	</div>
	{#if dirty}
		<div class="alert alert-warning alert-soft text-sm">
			<span>{t('agents-test-unsaved')}</span>
			<button class="btn btn-sm" type="button" onclick={() => void onsave()}>{t('agents-save')}</button>
		</div>
	{/if}

	<div class="grid gap-4 lg:grid-cols-[3fr_2fr]">
		<div class="flex min-h-96 flex-col rounded-box border border-base-300">
			<div class="flex-1 space-y-2 overflow-y-auto p-3" aria-live="polite">
				{#each messages as message, i (i)}
					<div class="chat {message.role === 'visitor' ? 'chat-end' : 'chat-start'}">
						<div class="chat-header text-xs opacity-60">
							{message.role === 'visitor' ? t('agents-test-visitor') : t('agents-test-agent')}
						</div>
						{#if message.role === 'agent'}
							<button
								class="chat-bubble cursor-pointer text-left whitespace-pre-wrap {message.error ? 'chat-bubble-error' : 'chat-bubble-primary'} {selected === i ? 'outline-2 outline-offset-2 outline-base-content/40' : ''}"
								type="button"
								onclick={() => (selected = i)}
								title={t('agents-test-show-debug')}
							>{message.text || (message.error ? '' : '…')}{#if message.error}{message.error}{/if}</button>
							{#if message.status && message.status !== 'completed'}
								<div class="chat-footer text-xs opacity-70">{t(testTurnLabel(message.status))}</div>
							{/if}
							{#if message.status === 'suspended' && message.suspension}
								{@const waiting = message.suspension}
								<div class="card card-border bg-base-200 mt-1 w-full max-w-md">
									<div class="card-body gap-2 p-3 text-sm">
										<p>{t(suspensionLabel(waiting.kind), { tool: waiting.tool ?? '' })}</p>
										{#if waiting.message}<p class="text-base-content/70">{waiting.message}</p>{/if}
										<p class="text-xs text-base-content/60">
											{t('agents-test-expires', { at: new Date(waiting.expires_at).toLocaleString() })}
										</p>
										{#if waiting.options.includes('value')}
											{@const field = answerField(waiting.kind)}
											<form
												class={field.secret ? 'join w-full' : 'flex w-full flex-col gap-2'}
												onsubmit={(e) => { e.preventDefault(); void answer(i, { decision: 'value', value: secret }); }}
											>
												{#if field.secret}
													<input
														class="input input-sm join-item w-full"
														type="password"
														autocomplete="off"
														bind:value={secret}
														aria-label={t(field.label)}
														placeholder={t(field.label)}
													/>
												{:else}
													<textarea class="textarea textarea-sm w-full" rows="3" bind:value={secret} aria-label={t(field.label)} placeholder={t(field.label)}></textarea>
												{/if}
												<button class={field.secret ? 'btn btn-sm btn-primary join-item' : 'btn btn-sm btn-primary self-end'} type="submit" disabled={busy || !secret}>
													{t(field.submit)}
												</button>
											</form>
										{/if}
										<div class="card-actions justify-end">
											{#if waiting.options.includes('allow_once')}
												<button class="btn btn-sm btn-primary" type="button" disabled={busy} onclick={() => void answer(i, { decision: 'allow_once' })}>
													{t('agents-test-approve')}
												</button>
											{/if}
											<button class="btn btn-sm btn-ghost" type="button" disabled={busy} onclick={() => void answer(i, { decision: 'deny' })}>
												{t('agents-test-deny')}
											</button>
										</div>
									</div>
								</div>
							{/if}
						{:else}
							<div class="chat-bubble whitespace-pre-wrap">{message.text}</div>
						{/if}
					</div>
				{:else}
					<p class="p-4 text-sm text-base-content/60">{t('agents-test-empty')}</p>
				{/each}
				{#if busy}<div class="skeleton h-10 w-2/3"></div>{/if}
			</div>
			{#if error}<div class="alert alert-error mx-3 mb-2 text-sm" role="alert"><span>{error}</span></div>{/if}
			<form class="flex gap-2 border-t border-base-300 p-3" onsubmit={(e) => { e.preventDefault(); void send(); }}>
				<textarea
					class="textarea min-h-12 w-full"
					bind:value={draft}
					placeholder={t('agents-test-placeholder')}
					aria-label={t('agents-test-placeholder')}
					onkeydown={(e) => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); void send(); } }}
				></textarea>
				<button class="btn btn-primary self-end" type="submit" disabled={busy || !draft.trim()}>{t('agents-test-send')}</button>
			</form>
		</div>

		<div class="rounded-box border border-base-300 p-3">
			<h3 class="mb-3 font-semibold">{t('agents-debug-heading')}</h3>
			{#if shown}
				<DebugPanel debug={shown} />
			{:else}
				<p class="text-sm text-base-content/60">{t('agents-debug-empty')}</p>
			{/if}
		</div>
	</div>
</div>
