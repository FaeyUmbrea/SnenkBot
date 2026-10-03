<script lang="ts">
	import DesktopShell from '../src/lib/DesktopShell.svelte';
	import type { ComponentProps } from 'svelte';
	let { props }: { props: Omit<ComponentProps<typeof DesktopShell>, 'children'> } = $props();
</script>

<DesktopShell {...props}
	><section aria-label="Active view">
		<input aria-label="Unsaved title" value="Sunday stream" />
	</section></DesktopShell
>
