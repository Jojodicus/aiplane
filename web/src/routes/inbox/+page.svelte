<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { base } from '$app/paths';
	import { page } from '$app/state';
	import {
		answerFor,
		errorMessage,
		focused,
		inboxApi,
		itemHeading,
		itemLink,
		kindLabel,
		minutesLeft,
		prettyArguments,
		slotText,
		type DecisionKind,
		type InboxItem
	} from '$lib/inbox';
	import { inboxLive, watchInbox } from '$lib/inbox.svelte';
	import { dt, t } from '$lib/i18n.svelte';

	let items = $state<InboxItem[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let answers = $state<Record<string, string>>({});
	let itemErrors = $state<Record<string, string>>({});
	let sent = $state<Record<string, boolean>>({});
	let busy = $state<string | null>(null);
	let highlighted = $state<string | null>(null);
	let scrolled = false;

	async function load() {
		try {
			items = await inboxApi.list();
			error = null;
		} catch (err) {
			error = errorMessage(err).message;
		} finally {
			loading = false;
		}
		highlighted = focused(items, page.url.searchParams);
		if (highlighted && !scrolled) {
			scrolled = true;
			await tick();
			document.getElementById(`inbox-item-${highlighted}`)?.scrollIntoView({ block: 'center' });
		}
	}

	onMount(() => {
		watchInbox();
		void load();
	});

	let seen = 0;
	$effect(() => {
		const version = inboxLive.version;
		if (version !== seen) {
			seen = version;
			void load();
		}
	});

	async function decide(item: InboxItem, decision: DecisionKind) {
		const answer = answerFor(decision, answers[item.id] ?? '');
		if (!answer) {
			itemErrors[item.id] = t('inbox-answer-required');
			return;
		}
		busy = item.id;
		delete itemErrors[item.id];
		try {
			await inboxApi.answer(item.id, answer);
			sent[item.id] = true;
			await load();
		} catch (err) {
			const { code, message } = errorMessage(err);
			itemErrors[item.id] = code === 'not_suspended' || code === 'inbox_item_not_found' ? t('inbox-already-settled') : message;
			if (code === 'not_suspended' || code === 'inbox_item_not_found') await load();
		} finally {
			busy = null;
		}
	}
</script>

<div class="w-full space-y-6">
	<header>
		<h1 class="text-2xl font-bold">{t('inbox-heading')}</h1>
		<p class="mt-2 max-w-3xl text-sm text-base-content/60">{t('inbox-intro')}</p>
	</header>

	{#if error}<div class="alert alert-error" role="alert"><span>{error}</span></div>{/if}

	{#if loading}
		<div class="skeleton h-28 w-full"></div>
	{:else if !items.length && !error}
		<div class="card card-border"><div class="card-body text-sm text-base-content/60">{t('inbox-empty')}</div></div>
	{:else}
		<ul class="flex flex-col gap-3">
			{#each items as item (item.id)}
				{@const heading = itemHeading(item)}
				{@const link = itemLink(item, base)}
				{@const left = minutesLeft(item.expires_at)}
				<li id="inbox-item-{item.id}" class="card card-border {highlighted === item.id ? 'border-primary' : ''}">
					<div class="card-body gap-3 p-4">
						<div class="flex flex-wrap items-start justify-between gap-2">
							<div class="min-w-0">
								<h2 class="card-title text-base break-words">{t(heading.key, { name: heading.name })}</h2>
								<div class="mt-1 flex flex-wrap items-center gap-2 text-xs text-base-content/60">
									<span class="badge badge-sm {item.kind === 'approval' ? 'badge-warning' : 'badge-info'}">{t(kindLabel(item.kind))}</span>
									{#if item.context?.inbox}<span class="badge badge-outline badge-sm">{item.context.inbox}</span>{/if}
									<span>{t('inbox-asked-at', { date: dt(item.created_at) })}</span>
									{#if left !== null}<span>{t('inbox-expires-in', { minutes: left })}</span>{/if}
								</div>
							</div>
							{#if link}<a class="btn btn-ghost btn-sm" href={link.href}>{t(link.key)}</a>{/if}
						</div>

						{#if item.kind === 'human_answer'}
							{#if item.question}
								<div>
									<p class="text-xs font-semibold text-base-content/60">{t('inbox-question')}</p>
									<p class="whitespace-pre-wrap break-words">{item.question}</p>
								</div>
							{/if}
							{#if item.context?.visitor_message}
								<div>
									<p class="text-xs font-semibold text-base-content/60">{t('inbox-visitor-message')}</p>
									<blockquote class="whitespace-pre-wrap break-words border-l-2 border-base-300 pl-3 text-sm">{item.context.visitor_message}</blockquote>
								</div>
							{/if}
							{#if item.context?.slots?.length}
								<div>
									<p class="text-xs font-semibold text-base-content/60">{t('inbox-slots')}</p>
									<ul class="mt-1 flex flex-wrap gap-2">
										{#each item.context.slots as slot (slot.slot)}
											{@const value = slotText(slot)}
											<li class="badge badge-ghost badge-sm h-auto py-1 break-all">
												<span class="font-mono">{slot.slot}</span>:
												{value ?? t('inbox-slot-trusted', { by: slot.set_by ?? '' })}
											</li>
										{/each}
									</ul>
								</div>
							{/if}
							{#if item.context?.transcript?.length}
								<details class="collapse collapse-arrow border border-base-300">
									<summary class="collapse-title text-sm font-semibold">{t('inbox-transcript')}</summary>
									<div class="collapse-content space-y-2 text-sm">
										{#each item.context.transcript as line, i (i)}
											<p class="whitespace-pre-wrap break-words">
												<span class="font-semibold">{t(line.role === 'user' ? 'inbox-transcript-visitor' : 'inbox-transcript-agent')}:</span>
												{line.text}
											</p>
										{/each}
									</div>
								</details>
							{/if}
						{:else if item.call}
							<div>
								<p class="text-xs font-semibold text-base-content/60">{t('inbox-tool', { tool: item.call.name })}</p>
								<pre class="mt-1 overflow-x-auto rounded-box bg-base-200 p-2 text-xs">{prettyArguments(item.call.arguments)}</pre>
							</div>
						{/if}

						{#if itemErrors[item.id]}<div class="alert alert-error alert-soft text-sm" role="alert"><span>{itemErrors[item.id]}</span></div>{/if}
						{#if sent[item.id]}<div class="alert alert-success alert-soft text-sm" role="status"><span>{t('inbox-sent')}</span></div>{/if}

						<div class="flex flex-col gap-2">
							{#if item.options.includes('value')}
								<label class="flex flex-col gap-1">
									<span class="text-sm">{t('inbox-answer-label')}</span>
									<textarea class="textarea w-full" rows="3" bind:value={answers[item.id]} disabled={busy === item.id}></textarea>
								</label>
							{/if}
							<div class="flex flex-wrap gap-2">
								{#if item.options.includes('allow_once')}
									<button class="btn btn-primary btn-sm" type="button" disabled={busy === item.id} onclick={() => decide(item, 'allow_once')}>{t('inbox-approve')}</button>
								{/if}
								{#if item.options.includes('value')}
									<button class="btn btn-primary btn-sm" type="button" disabled={busy === item.id} onclick={() => decide(item, 'value')}>{t('inbox-send-answer')}</button>
								{/if}
								{#if item.options.includes('deny')}
									<button class="btn btn-ghost btn-sm" type="button" disabled={busy === item.id} onclick={() => decide(item, 'deny')}>{t(item.kind === 'approval' ? 'inbox-deny' : 'inbox-decline')}</button>
								{/if}
							</div>
						</div>
					</div>
				</li>
			{/each}
		</ul>
	{/if}
</div>
