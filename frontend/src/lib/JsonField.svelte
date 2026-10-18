<script lang="ts">
	const instanceId = $props.id();
	/** A reusable JSON editor that commits only valid values to its owning form. */
	let {
		label,
		id = instanceId,
		value,
		disabled = false,
		onChange
	}: {
		label: string;
		id?: string;
		value: unknown;
		disabled?: boolean;
		onChange: (value: unknown) => void | Promise<void>;
	} = $props();
	let draft = $derived(JSON.stringify(value, null, 2) ?? 'null');
	let error = $state('');
	let control: HTMLTextAreaElement;
	$effect(() => {
		JSON.stringify(value);
		error = '';
		control?.setCustomValidity('');
	});

	/** Keep incomplete edits local, then commit parsed JSON when the editor loses focus. */
	async function change(event: Event) {
		const control = event.currentTarget as HTMLTextAreaElement;
		draft = control.value;
		let parsed: unknown;
		try {
			parsed = JSON.parse(draft);
		} catch {
			error = 'Enter valid JSON.';
			control.setCustomValidity(error);
			return;
		}
		error = '';
		control.setCustomValidity('');
		await onChange(parsed);
	}
</script>

<div class="json-field">
	<textarea
		{id}
		aria-label={label}
		bind:this={control}
		aria-invalid={Boolean(error)}
		aria-describedby={error ? `${id}-error` : undefined}
		value={draft}
		{disabled}
		spellcheck="false"
		rows="6"
		onchange={change}></textarea>
	{#if error}<p id={`${id}-error`} role="alert">{error}</p>{/if}
</div>

<style>
	.json-field {
		min-width: 0;
		width: 100%;
	}
	textarea {
		box-sizing: border-box;
		width: 100%;
		resize: vertical;
		font-family: ui-monospace, monospace;
		color: var(--text);
		background: var(--field);
		border: 1px solid var(--border);
		border-radius: 6px;
		padding: 8px;
		line-height: 1.5;
	}
	p {
		margin: 0.35rem 0 0;
		color: var(--gold);
	}
</style>
