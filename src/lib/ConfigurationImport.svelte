<script lang="ts">
	import { DesktopRequestError } from './desktop';
	let {
		label,
		description,
		maxBytes,
		onImport
	}: {
		label: string;
		description: string;
		maxBytes: number;
		onImport: (value: unknown) => Promise<string>;
	} = $props();
	let busy = $state(false);
	let message = $state('');
	let error = $state('');
	/** Parse a user-selected JSON document within the integration's size limit. */
	async function read(event: Event) {
		const input = event.currentTarget as HTMLInputElement;
		const file = input.files?.[0];
		if (!file || busy) return;
		busy = true;
		message = '';
		error = '';
		try {
			if (file.size > maxBytes) {
				error = `The file exceeds the ${maxBytes} byte limit.`;
				return;
			}
			let value: unknown;
			try {
				value = JSON.parse(await file.text());
			} catch {
				error = 'Choose a valid JSON file.';
				return;
			}
			message = await onImport(value);
		} catch (cause) {
			error =
				cause instanceof DesktopRequestError
					? cause.message
					: 'The document could not be imported.';
		} finally {
			busy = false;
			input.value = '';
		}
	}
</script>

<details>
	<summary>{label}</summary>
	<p>{description}</p>
	<input
		aria-label={label}
		type="file"
		accept="application/json,.json"
		disabled={busy}
		onchange={read}
	/>{#if message}<p role="status">{message}</p>{/if}{#if error}<p role="alert">{error}</p>{/if}
</details>
