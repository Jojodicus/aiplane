<script lang="ts">
	import { onMount } from 'svelte';
	import { adminJson } from '$lib/admin-client';
	import { extensionStatus, onExtensionState, requestActivation, type ExtensionStatus } from '$lib/browser-bridge';
	import { browserSetupStage, chromeWebStoreUrl, EXTENSION_DOWNLOAD_PATH } from '$lib/browser-extension';
	import { t } from '$lib/i18n.svelte';
	import type { ToolEntry, ToolsResponse } from '$lib/tools';

	let tool = $state<ToolEntry | undefined>(undefined);
	let loaded = $state(false);
	let error = $state<string | null>(null);
	let extension = $state<ExtensionStatus>({ present: false, armed: false });
	let checking = $state(false);
	let popupRefused = $state(false);
	let copied = $state(false);
	let origin = $state('');

	let stage = $derived(browserSetupStage(tool, extension));

	const statusView = {
		not_granted: { alert: 'alert-warning', key: 'tools-browser-status-not-granted' },
		tool_off: { alert: 'alert-warning', key: 'tools-browser-status-tool-off' },
		not_detected: { alert: 'alert-info', key: 'tools-browser-status-not-detected' },
		switched_off: { alert: 'alert-info', key: 'tools-browser-status-switched-off' },
		ready: { alert: 'alert-success', key: 'tools-browser-status-ready' }
	} as const;

	async function loadTool() {
		try {
			const data = await adminJson<ToolsResponse>('/api/v0/tools');
			tool = data.tools.find((entry) => entry.key === 'browser_control');
			error = null;
		} catch (err) {
			error = String(err);
		} finally {
			loaded = true;
		}
	}

	async function recheck() {
		checking = true;
		extension = await extensionStatus();
		checking = false;
	}

	async function switchOn() {
		popupRefused = false;
		const { opened } = await requestActivation();
		popupRefused = !opened;
	}

	async function copyOrigin() {
		try {
			await navigator.clipboard.writeText(origin);
			copied = true;
			setTimeout(() => (copied = false), 2000);
		} catch {
			copied = false;
		}
	}

	onMount(() => {
		origin = window.location.origin;
		void loadTool();
		void recheck();
		return onExtensionState((status) => {
			extension = status;
			if (status.armed) popupRefused = false;
		});
	});
</script>

<div class="flex w-full flex-col gap-6">
	<p class="m-0 text-sm text-base-content/60">{t('tools-browser-description')}</p>

	{#if error}
		<div class="alert alert-error"><span>{error}</span></div>
	{/if}

	<section class="card border border-base-300">
		<div class="card-body">
			<h2 class="card-title text-base">{t('tools-browser-status-heading')}</h2>
			{#if loaded}
				<div role="status" class="alert {statusView[stage].alert}">
					<span>{t(statusView[stage].key)}</span>
				</div>
			{:else}
				<span class="loading loading-spinner loading-sm"></span>
			{/if}
			<div class="mt-2 flex flex-wrap items-center gap-3">
				{#if stage === 'switched_off'}
					<button type="button" class="btn btn-primary btn-sm" onclick={switchOn}>{t('tools-browser-switch-on')}</button>
				{/if}
				{#if stage === 'tool_off'}
					<a class="btn btn-primary btn-sm" href="/tools">{t('tools-browser-open-tools')}</a>
				{/if}
				{#if stage !== 'not_granted' && stage !== 'ready'}
					<button type="button" class="btn btn-ghost btn-sm" onclick={recheck} disabled={checking}>{t('tools-browser-recheck')}</button>
				{/if}
			</div>
			{#if popupRefused}
				<p class="m-0 text-sm text-base-content/60">{t('tools-browser-switch-on-fallback')}</p>
			{/if}
		</div>
	</section>

	<section class="card border border-base-300">
		<div class="card-body">
			<h2 class="card-title text-base">{t('tools-browser-install-heading')}</h2>
			<div class="flex flex-wrap items-center gap-3">
				<a class="btn btn-primary btn-sm" href={chromeWebStoreUrl} target="_blank" rel="noopener noreferrer">{t('tools-browser-store-button')}</a>
				<a class="btn btn-outline btn-sm" href={EXTENSION_DOWNLOAD_PATH} download>{t('tools-browser-download-button')}</a>
			</div>
			<p class="m-0 text-sm text-base-content/60">{t('tools-browser-download-note')}</p>

			<h3 class="mt-4 font-semibold">{t('tools-browser-steps-heading')}</h3>
			<ol class="list-decimal space-y-2 pl-5 text-sm">
				<li>{t('tools-browser-step-install')}</li>
				<li>
					{t('tools-browser-step-pair')}
					<div class="join mt-2 flex w-full max-w-md">
						<input class="input input-sm join-item w-full font-mono" readonly value={origin} aria-label={t('tools-browser-origin-label')} />
						<button type="button" class="btn btn-sm join-item" onclick={copyOrigin}>
							{copied ? t('tools-browser-copied') : t('tools-browser-copy-origin')}
						</button>
					</div>
				</li>
				<li>{t('tools-browser-step-switch-on')}</li>
				<li>{t('tools-browser-step-ask')}</li>
			</ol>

			<h3 class="mt-4 font-semibold">{t('tools-browser-unpacked-heading')}</h3>
			<p class="m-0 text-sm">{t('tools-browser-unpacked-steps')}</p>
		</div>
	</section>

	<section class="card border border-base-300">
		<div class="card-body">
			<h2 class="card-title text-base">{t('tools-browser-notes-heading')}</h2>
			<ul class="list-disc space-y-2 pl-5 text-sm">
				<li>{t('tools-browser-note-window')}</li>
				<li>{t('tools-browser-note-open')}</li>
				<li>{t('tools-browser-note-injection')}</li>
			</ul>
		</div>
	</section>
</div>
