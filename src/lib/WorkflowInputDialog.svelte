<script lang="ts">
	import { onDestroy } from 'svelte';
	import type { InputRequest, InputRequestIdentity, SubmitInput } from './contracts/index';
	import './theme.css';

	let {
		request,
		onSubmit,
		onCancel
	}: {
		request: InputRequest | null;
		onSubmit: (input: SubmitInput) => Promise<void>;
		onCancel: (identity: InputRequestIdentity) => Promise<void>;
	} = $props();

	const titleId = $props.id();
	let dialog: HTMLDialogElement;
	let draft = $state<Record<string, string>>({});
	let submitting = $state(false);
	let submitted = $state(false);
	let cancelling = $state(false);
	let localError = $state<string | null>(null);
	let previousRequest: InputRequest | null = null;
	let previousValidating = false;
	let previousError: string | null = null;
	let generation = 0;
	const busy = $derived(submitting || submitted || request?.validating || cancelling);
	const error = $derived(localError ?? request?.error);

	onDestroy(() => {
		generation += 1;
		if (dialog?.open) dialog.close();
	});

	function textValue(value: unknown): string {
		if (value === null || value === undefined) return '';
		if (typeof value === 'string') return value;
		return JSON.stringify(value);
	}

	$effect(() => {
		const current = request;
		const validating = current?.validating ?? false;
		const validationError = current?.error ?? null;
		if (current?.request_id !== previousRequest?.request_id) {
			generation += 1;
			draft = Object.fromEntries(
				(current?.fields ?? []).map((field) => [
					field.id,
					textValue(
						Object.hasOwn(current?.values ?? {}, field.id)
							? current?.values[field.id]
							: current?.defaults[field.id]
					)
				])
			);
			submitting = false;
			submitted = false;
			cancelling = false;
			localError = null;
		} else if (
			current !== previousRequest ||
			validating !== previousValidating ||
			validationError !== previousError
		) {
			localError = null;
			if (!validating) submitted = false;
		}
		previousRequest = current;
		previousValidating = validating;
		previousError = validationError;
		if (current && dialog && !dialog.open) dialog.showModal();
		if (!current && dialog?.open) dialog.close();
	});

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		if (!request || busy) return;
		const identity = request.request_id;
		const owner = generation;
		const values = Object.fromEntries(request.fields.map((field) => [field.id, draft[field.id]]));
		submitting = true;
		submitted = true;
		localError = null;
		try {
			await onSubmit({ request_id: identity, values });
		} catch {
			if (request?.request_id === identity && generation === owner) {
				submitted = false;
				localError = 'Could not submit your input. Try again.';
			}
		} finally {
			if (request?.request_id === identity && generation === owner) submitting = false;
		}
	}

	async function cancel() {
		if (!request || cancelling) return;
		const identity = request.request_id;
		const owner = generation;
		cancelling = true;
		localError = null;
		try {
			await onCancel({ request_id: identity });
		} catch {
			if (request?.request_id === identity && generation === owner) {
				cancelling = false;
				localError = 'Could not cancel the run. Try again.';
			}
		}
	}
</script>

<dialog
	class="workflow-editor"
	bind:this={dialog}
	aria-labelledby={titleId}
	oncancel={(event) => {
		event.preventDefault();
		void cancel();
	}}
>
	{#if request}
		<form onsubmit={submit} novalidate>
			<header><h2 id={titleId}>{request.title || 'Workflow input'}</h2></header>
			<div class="fields">
				{#each request.fields as field, index (field.id)}
					<label>
						<span
							>{field.label?.trim() || `Input ${index + 1}`}{#if field.required}<span
									class="required">Required</span
								>{/if}</span
						>
						<input type="text" aria-required={field.required} bind:value={draft[field.id]} />
					</label>
				{/each}
			</div>
			{#if error}<p class="error" role="alert">{error}</p>{/if}
			<footer>
				{#if busy}<span role="status">{cancelling ? 'Cancelling…' : 'Checking input…'}</span>{/if}
				<button type="button" disabled={cancelling} onclick={cancel}>Cancel</button>
				<button class="submit" type="submit" disabled={busy}>Continue</button>
			</footer>
		</form>
	{/if}
</dialog>

<style>
	dialog {
		width: min(520px, calc(100vw - 32px));
		max-height: min(560px, calc(100vh - 32px));
		padding: 0;
		background: var(--surface);
		color: var(--text);
		border: 1px solid var(--border);
		border-radius: 8px;
		font-family: 'Inter Tight', sans-serif;
		font-size: 14px;
	}
	dialog::backdrop {
		background: rgb(0 0 0 / 60%);
	}
	form {
		display: flex;
		flex-direction: column;
		max-height: min(558px, calc(100vh - 34px));
	}
	header,
	footer {
		padding: 16px 20px;
		flex: none;
	}
	h2 {
		font-size: 17px;
		font-weight: 600;
		margin: 0;
		overflow-wrap: anywhere;
	}
	.fields {
		display: grid;
		gap: 12px;
		padding: 0 20px 4px;
		overflow-y: auto;
		min-height: 0;
	}
	label {
		display: grid;
		gap: 6px;
		min-width: 0;
	}
	label > span {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 8px;
		overflow-wrap: anywhere;
	}
	.required,
	footer > span {
		color: var(--muted);
		font-size: 12px;
	}
	input,
	button {
		font: inherit;
		color: inherit;
		min-height: 32px;
		border: 1px solid var(--border);
		border-radius: 6px;
		padding: 6px 10px;
		background: var(--field);
	}
	input {
		width: 100%;
		min-width: 0;
	}
	footer {
		display: flex;
		justify-content: flex-end;
		align-items: center;
		gap: 8px;
	}
	footer > span {
		margin-right: auto;
	}
	.submit {
		background: var(--blood);
	}
	button:disabled {
		opacity: 0.5;
		cursor: default;
	}
	.error {
		color: var(--gold);
		margin: 12px 20px 0;
		overflow-wrap: anywhere;
		flex: none;
		max-height: min(120px, 20vh);
		overflow-y: auto;
	}
</style>
