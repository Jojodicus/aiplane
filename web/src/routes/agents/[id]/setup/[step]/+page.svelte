<script lang="ts">
	import { page } from '$app/state';
	import SetupAssistant from '#lib/components/agents/setup/SetupAssistant.svelte';
</script>

<SetupAssistant step={page.params.step ?? ''} />
