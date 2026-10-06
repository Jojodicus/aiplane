<script lang="ts">
	import type { Spec } from '#lib/agents.js';
	import { publishState, readScope, type StepKey, type Todo } from '#lib/agent-setup.js';
	import { t } from '#lib/i18n.svelte.js';

	/**
	 * "Before you publish": every open item with a link to the step that fixes
	 * it (the advanced editor for what no step reaches), then what is in place.
	 */
	let { todos, spec, onfix }: { todos: Todo[]; spec: Spec; onfix: (step: StepKey | null) => void } = $props();

	const scope = $derived(readScope(spec));
	const state = $derived(publishState(todos));
	const text = (todo: Todo) => (todo.key ? t(todo.key, todo.args) : (todo.message ?? ''));
</script>

<ul class="m-0 flex list-none flex-col gap-2 p-0 text-sm">
	{#each todos as todo, i (i)}
		<li class="flex items-start gap-2">
			<span class="badge badge-soft rounded-full px-1.5 {todo.blocking ? 'badge-warning' : 'badge-info'}" aria-hidden="true">!</span>
			<span class="min-w-0 break-words">
				{text(todo)}
				<button class="link link-primary" type="button" onclick={() => onfix(todo.step)}>
					{todo.step ? t('agents-setup-fix') : t('agents-setup-fix-advanced')}
				</button>
			</span>
		</li>
	{/each}
	{#if scope.topics.length}
		<li class="flex items-start gap-2">
			<span class="badge badge-soft badge-success rounded-full px-1.5" aria-hidden="true">✓</span>
			<span>{t(scope.strict ? 'agents-setup-check-scope-strict' : 'agents-setup-check-scope', { count: scope.topics.length })}</span>
		</li>
	{/if}
	{#if state !== 'blocked'}
		<li class="flex items-start gap-2">
			<span class="badge badge-soft badge-success rounded-full px-1.5" aria-hidden="true">✓</span>
			<span>{t(state === 'ready' ? 'agents-setup-ready' : 'agents-setup-ready-recommended')}</span>
		</li>
	{/if}
</ul>
