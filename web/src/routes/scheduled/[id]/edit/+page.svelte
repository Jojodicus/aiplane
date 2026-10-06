<script lang="ts">
	import { page } from '$app/state';
	import ScheduleEditorPage from '#lib/components/scheduled/ScheduleEditorPage.svelte';
</script>

<ScheduleEditorPage id={page.params.id} />
