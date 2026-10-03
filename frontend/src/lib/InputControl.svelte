<script lang="ts">
	import type { ConfigFieldKind, ConfigSchema, Input, Step } from './contracts/index';
	import type { CanvasEffect, WorkflowIndex } from './canvas';
	import { referenceTitle } from './canvas';
	import Icon from './Icon.svelte';

	let {
		input,
		label,
		hideLabel = false,
		kind = 'text',
		steps,
		schemas,
		index,
		workflowTitles = {},
		disabled = false,
		onChange,
		onSourcePicker
	}: {
		input: Input;
		label: string;
		hideLabel?: boolean;
		kind?: ConfigFieldKind;
		steps: readonly Step[];
		schemas: readonly ConfigSchema[];
		index?: WorkflowIndex;
		workflowTitles?: Readonly<Record<string, string>>;
		disabled?: boolean;
		onChange: (value: Input) => CanvasEffect;
		onSourcePicker: () => void;
	} = $props();

	let invalid = $state(false);
	const literalText = $derived(input.Literal === null ? '' : String(input.Literal ?? ''));
	const literalToggle = $derived(input.Literal === true);
	let draft = $derived(literalText);
	let toggleDraft = $derived(literalToggle);
	const isLiteral = $derived(Object.hasOwn(input, 'Literal'));
	const isSimple = $derived(
		isLiteral &&
			(input.Literal === null || ['string', 'number', 'boolean'].includes(typeof input.Literal))
	);
	const isToggle = $derived(isSimple && (kind === 'toggle' || typeof input.Literal === 'boolean'));
	const inputType = $derived(
		kind === 'secret'
			? 'password'
			: kind === 'integer' || typeof input.Literal === 'number'
				? 'number'
				: 'text'
	);

	async function changeLiteral(event: Event) {
		const control = event.currentTarget;
		if (!(control instanceof HTMLInputElement)) return;
		if (isToggle) {
			toggleDraft = control.checked;
			await onChange({ Literal: toggleDraft });
			return;
		}
		if (inputType === 'number') {
			draft = control.value;
			const value = control.valueAsNumber;
			invalid = !Number.isFinite(value) || (kind === 'integer' && !Number.isSafeInteger(value));
			if (!invalid) await onChange({ Literal: value });
			return;
		}
		invalid = false;
		draft = control.value;
		await onChange({ Literal: draft });
	}
</script>

<div class="input-control">
	{#if !hideLabel}<span class="field-label">{label}</span>{/if}
	<div class="value-row">
		{#if isToggle}
			<label class="toggle">
				<input
					type="checkbox"
					aria-label={label}
					checked={toggleDraft}
					{disabled}
					onchange={changeLiteral}
				/>
				<span>{toggleDraft ? 'On' : 'Off'}</span>
			</label>
		{:else if isSimple}
			<input
				class="literal"
				type={inputType}
				aria-label={label}
				aria-invalid={invalid}
				size={Math.max(3, Math.min(draft.length, 32))}
				step={kind === 'integer' ? 1 : 'any'}
				value={draft}
				{disabled}
				onchange={changeLiteral}
			/>
		{:else if input.Text !== undefined}
			<button class="text-value" aria-label={`Edit ${label}`} {disabled} onclick={onSourcePicker}>
				{#each input.Text as part, partIndex (partIndex)}
					{#if part.Literal !== undefined}<span>{part.Literal}</span>
					{:else if part.Value !== undefined}
						{#if Object.hasOwn(part.Value, 'Literal')}
							<span
								>{typeof part.Value.Literal === 'object'
									? 'Value'
									: String(part.Value.Literal)}</span
							>
						{:else}<span class="token"
								>{referenceTitle(part.Value, steps, schemas, workflowTitles, index)}</span
							>{/if}
					{/if}
				{/each}
			</button>
		{:else}
			<button
				class:reference={input.Reference !== undefined ||
					input.Variable !== undefined ||
					input.Trigger !== undefined}
				class="source-value"
				aria-label={`Edit ${label} source`}
				{disabled}
				onclick={onSourcePicker}
				>{referenceTitle(input, steps, schemas, workflowTitles, index)}</button
			>
		{/if}
		<button
			class="source-picker"
			aria-label={`Choose source for ${label}`}
			title="Choose a value or an action output"
			{disabled}
			onclick={onSourcePicker}><Icon name="pencil" size={14} /></button
		>
	</div>
	{#if invalid}<span class="validation"
			>Enter a valid {kind === 'integer' ? 'whole number' : 'number'}.</span
		>{/if}
</div>

<style>
	.input-control {
		display: flex;
		align-items: center;
		gap: 8px;
		min-width: 0;
		max-width: 100%;
	}
	.field-label {
		color: var(--muted);
		font-size: 12px;
		flex: none;
	}
	.value-row {
		display: flex;
		gap: 4px;
		align-items: stretch;
		min-width: 0;
		flex: 0 1 auto;
	}
	.literal,
	.source-value,
	.text-value,
	.toggle {
		background: var(--field);
		border: 1px solid var(--border);
		border-radius: 6px;
		min-height: 32px;
		min-width: 48px;
		max-width: 100%;
		padding: 6px 9px;
	}
	.literal {
		width: auto;
		max-width: 320px;
	}
	.literal[type='number'] {
		width: 90px;
	}
	.source-value,
	.text-value {
		text-align: left;
		overflow-wrap: anywhere;
	}
	.reference,
	.token {
		color: var(--amber);
	}
	.reference {
		border-color: var(--amber);
	}
	.text-value {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 4px;
		white-space: pre-wrap;
	}
	.token {
		background: var(--side);
		border: 1px solid var(--amber);
		border-radius: 4px;
		padding: 2px 5px;
	}
	.source-picker {
		background: none;
		border: 1px solid transparent;
		border-radius: 6px;
		display: grid;
		place-items: center;
		width: 24px;
		flex: none;
		color: var(--muted);
	}
	.source-picker:hover {
		border-color: var(--amber);
	}
	.toggle {
		display: flex;
		gap: 8px;
		align-items: center;
	}
	.toggle input {
		margin: 0;
		width: 17px;
		height: 17px;
	}
	.validation {
		color: var(--gold);
		font-size: 12px;
	}
</style>
