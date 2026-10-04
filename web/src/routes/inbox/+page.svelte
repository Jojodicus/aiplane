<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { base } from '$app/paths';
	import { page } from '$app/state';
	import { errorMessage, focused, inboxApi, itemHeading, itemLink, kindLabel, type InboxItem } from '$lib/inbox';
	import { inboxLive, watchInbox } from '$lib/inbox.svelte';
	import type { Answer } from '$lib/suspension';
	import { dt, t } from '$lib/i18n.svelte';
	import SuspensionCard from '$lib/components/SuspensionCard.svelte';

	let items = $state<InboxItem[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
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

	async function decide(item: InboxItem, answer: Answer) {
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
				<li>
					<SuspensionCard
						id="inbox-item-{item.id}"
						class={highlighted === item.id ? 'border-primary' : ''}
						waiting={item}
						busy={busy === item.id}
						error={itemErrors[item.id] ?? null}
						onanswer={(answer) => decide(item, answer)}
					>
						<div class="flex flex-wrap items-start justify-between gap-2">
							<div class="min-w-0">
								<h2 class="card-title text-base break-words">{t(heading.key, { name: heading.name })}</h2>
								<div class="mt-1 flex flex-wrap items-center gap-2 text-xs text-base-content/60">
									<span class="badge badge-sm {item.kind === 'approval' ? 'badge-warning' : 'badge-info'}">{t(kindLabel(item.kind))}</span>
									{#if item.context?.inbox}<span class="badge badge-outline badge-sm">{item.context.inbox}</span>{/if}
									<span>{t('inbox-asked-at', { date: dt(item.created_at) })}</span>
								</div>
							</div>
							{#if link}<a class="btn btn-ghost btn-sm" href={link.href}>{t(link.key)}</a>{/if}
						</div>
						{#if sent[item.id]}<div class="alert alert-success alert-soft" role="status"><span>{t('inbox-sent')}</span></div>{/if}
					</SuspensionCard>
				</li>
			{/each}
		</ul>
	{/if}
</div>
