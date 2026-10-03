<script lang="ts">
	import type { Condition, ConfigSchema, Input, Step } from './contracts/index';
	import type { CanvasEffect, ConditionUpdate, WorkflowIndex } from './canvas';
	import InputControl from './InputControl.svelte';
	import ConditionControl from './ConditionControl.svelte';

	let {
		condition,
		steps,
		schemas,
		index,
		workflowTitles = {},
		disabled = false,
		onChange,
		onSourcePicker,
		path = 'condition'
	}: {
		condition: Condition;
		steps: readonly Step[];
		schemas: readonly ConfigSchema[];
		index?: WorkflowIndex;
		workflowTitles?: Readonly<Record<string, string>>;
		disabled?: boolean;
		onChange: (update: ConditionUpdate) => CanvasEffect;
		onSourcePicker: (path: string, input: Input) => void;
		path?: string;
	} = $props();

	const comparisons = {
		Equal: 'is',
		NotEqual: 'is not',
		Contains: 'contains',
		Less: 'is less than',
		Greater: 'is greater than'
	} as const;
	type Comparison = keyof typeof comparisons;
	const comparison = $derived(
		(Object.keys(comparisons) as Comparison[]).find((key) => condition[key] !== undefined)
	);
	const operands = $derived(comparison ? condition[comparison] : undefined);

	function changeOperand(index: number, value: Input): CanvasEffect {
		if (!comparison || !operands) return;
		const operator = comparison;
		return onChange((current) => {
			const currentOperands = current[operator];
			if (!currentOperands) return current;
			const next: [Input, Input] = [currentOperands[0], currentOperands[1]];
			next[index] = value;
			if (operator === 'Equal') return { Equal: next };
			if (operator === 'NotEqual') return { NotEqual: next };
			if (operator === 'Contains') return { Contains: next };
			if (operator === 'Less') return { Less: next };
			return { Greater: next };
		});
	}
</script>

{#snippet nested(value: Condition, suffix: string, update: (next: ConditionUpdate) => CanvasEffect)}
	<ConditionControl
		condition={value}
		{steps}
		{schemas}
		{index}
		{workflowTitles}
		{disabled}
		onChange={update}
		{onSourcePicker}
		path={`${path}.${suffix}`}
	/>
{/snippet}

<div class="condition-control">
	{#if comparison && operands}
		<InputControl
			hideLabel
			input={operands[0]}
			label="Value"
			{steps}
			{schemas}
			{index}
			{workflowTitles}
			{disabled}
			onChange={(value) => changeOperand(0, value)}
			onSourcePicker={() => onSourcePicker(`${path}.left`, operands[0])}
		/>
		<span class="operator">{comparisons[comparison]}</span>
		<InputControl
			hideLabel
			input={operands[1]}
			label="Compared with"
			{steps}
			{schemas}
			{index}
			{workflowTitles}
			{disabled}
			onChange={(value) => changeOperand(1, value)}
			onSourcePicker={() => onSourcePicker(`${path}.right`, operands[1])}
		/>
	{:else if condition.Exists !== undefined}
		{@const currentInput = condition.Exists}
		<InputControl
			hideLabel
			input={condition.Exists}
			label="Value"
			{steps}
			{schemas}
			{index}
			{workflowTitles}
			{disabled}
			onChange={(value) =>
				onChange((current) => (current.Exists !== undefined ? { Exists: value } : current))}
			onSourcePicker={() => onSourcePicker(path, currentInput)}
		/>
		<span class="operator">exists</span>
	{:else if condition.NullOrEmpty !== undefined}
		{@const currentInput = condition.NullOrEmpty}
		<InputControl
			hideLabel
			input={condition.NullOrEmpty}
			label="Value"
			{steps}
			{schemas}
			{index}
			{workflowTitles}
			{disabled}
			onChange={(value) =>
				onChange((current) =>
					current.NullOrEmpty !== undefined ? { NullOrEmpty: value } : current
				)}
			onSourcePicker={() => onSourcePicker(path, currentInput)}
		/>
		<span class="operator">is empty</span>
	{:else if condition.Not !== undefined}
		<span class="operator">Not</span>
		{@render nested(condition.Not, 'not', (update) =>
			onChange((current) => (current.Not !== undefined ? { Not: update(current.Not) } : current))
		)}
	{:else if condition.All !== undefined || condition.Any !== undefined}
		{@const conditions = condition.All ?? condition.Any ?? []}
		{@const isAll = condition.All !== undefined}
		<div class="condition-group">
			<span class="group-label">{isAll ? 'All conditions' : 'Any condition'}</span>
			{#each conditions as child, index (index)}
				{@render nested(child, String(index), (update) =>
					onChange((current) => {
						const currentConditions = isAll ? current.All : current.Any;
						if (!currentConditions?.[index]) return current;
						const updated = [...currentConditions];
						updated[index] = update(currentConditions[index]);
						return isAll ? { All: updated } : { Any: updated };
					})
				)}
			{/each}
		</div>
	{/if}
</div>

<style>
	.condition-control {
		display: flex;
		gap: 8px;
		align-items: center;
		flex-wrap: wrap;
	}
	.condition-control > :global(.input-control) {
		flex: 0 1 auto;
		min-width: 80px;
	}
	.operator {
		padding: 0;
		color: var(--muted);
	}
	.condition-group {
		display: flex;
		flex-direction: column;
		gap: 10px;
		width: 100%;
	}
	.group-label {
		color: var(--muted);
		font-size: 12px;
	}
</style>
