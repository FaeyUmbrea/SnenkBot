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
	let dialog: HTMLDialogElement;
	const titleId = $props.id();
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

<button type="button" onclick={() => dialog.showModal()}>{label}</button>
<dialog
	bind:this={dialog}
	aria-labelledby={titleId}
	oncancel={(event) => {
		if (busy) event.preventDefault();
	}}
>
	<h2 id={titleId}>{label}</h2>
	<p>{description}</p>
	<input
		aria-label={label}
		type="file"
		accept="application/json,.json"
		disabled={busy}
		onchange={read}
	/>{#if message}<p role="status">{message}</p>{/if}{#if error}<p role="alert">{error}</p>{/if}
	<footer>
		<button type="button" disabled={busy} onclick={() => dialog.close()}>Close</button>
	</footer>
</dialog>

<style>
	button {
		min-height: 32px;
		padding: 6px 12px;
		border: 1px solid var(--border);
		border-radius: 6px;
		background: var(--surface);
	}
	dialog {
		max-width: min(480px, calc(100vw - 32px));
		padding: 24px;
		background: var(--side);
		color: var(--text);
		border: 1px solid var(--border);
		border-radius: 8px;
	}
	dialog::backdrop {
		background: #0009;
	}
	h2 {
		margin: 0;
		font-size: 18px;
	}
	p {
		color: var(--muted);
		line-height: 1.5;
	}
	footer {
		display: flex;
		justify-content: flex-end;
		margin-top: 24px;
	}
	input {
		width: 100%;
	}
</style>
