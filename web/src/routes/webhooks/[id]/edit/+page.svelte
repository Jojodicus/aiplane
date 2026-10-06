<script lang="ts">
	import { page } from '$app/state';
	import WebhookEditorPage from '#lib/components/webhooks/WebhookEditorPage.svelte';
</script>

<WebhookEditorPage id={page.params.id} />
