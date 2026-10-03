<script lang="ts">
	import { onDestroy, tick } from 'svelte';
	import type { Input, Step, StepDestination, StepPosition } from './contracts/index';
	import type { CanvasEffect, CanvasProps, ConditionUpdate } from './canvas';
	import {
		actionSummaryFields,
		createWorkflowIndex,
		fieldTitle,
		sameDestination,
		schemaFor,
		stepTitle,
		validPlacement
	} from './canvas';
	import {
		currentWorkflowDrag,
		endWorkflowDrag,
		readWorkflowDrag,
		startWorkflowDrag,
		WORKFLOW_DRAG_TYPE
	} from './drag';
	import InputControl from './InputControl.svelte';
	import ConditionControl from './ConditionControl.svelte';
	import Icon from './Icon.svelte';
	import './theme.css';

	let {
		workflow,
		schemas,
		workflowTitles = {},
		catalog = [],
		summaryFields = {},
		selectedStepId = null,
		disabled = false,
		onSelect,
		onMove,
		onAdd,
		onInputChange,
		onEditStep,
		onSourcePicker,
		onInspectStep
	}: CanvasProps = $props();
	let canvas: HTMLDivElement;
	let pending = $state(false);
	let message = $state('');
	let moveStepId = $state<string | null>(null);
	let moveDestination = $state<StepDestination>('Root');
	let movePosition = $state<StepPosition>('Append');
	let dropTarget = $state<string | null>(null);
	let insertTarget = $state<{
		destination: StepDestination;
		position: StepPosition;
		key: string;
	} | null>(null);
	let scrollFrame = 0;
	let scrollSpeed = 0;
	let operations: Promise<void> = Promise.resolve();
	let pendingCount = 0;
	const unavailable = $derived(disabled || pending);
	const workflowIndex = $derived(createWorkflowIndex(workflow, schemas, workflowTitles));
	const lists = $derived(workflowIndex.lists);
	const moveList = $derived(
		lists.find((list) => sameDestination(list.destination, moveDestination))
	);
	const moveChoices = $derived(moveList?.steps.filter((step) => step.id !== moveStepId) ?? []);

	function placementKey(destination: StepDestination, position: StepPosition): string {
		const list =
			destination === 'Root'
				? 'root'
				: `${destination.Branch.parent_id}:${destination.Branch.branch}`;
		return `${list}:${position === 'Append' ? 'end' : position.Before.step_id}`;
	}

	function effect(operation: () => CanvasEffect, focusStepId?: string): Promise<void> {
		if (disabled) return Promise.resolve();
		const workflowId = workflow.id;
		pendingCount += 1;
		pending = true;
		message = '';
		const operationResult = operations
			.then(async () => {
				if (workflow.id !== workflowId) return;
				try {
					await operation();
					if (workflow.id !== workflowId) return;
					moveStepId = null;
					insertTarget = null;
					if (focusStepId) {
						onSelect(focusStepId);
						await tick();
						canvas
							.querySelector<HTMLButtonElement>(`[data-step-title="${CSS.escape(focusStepId)}"]`)
							?.focus();
					}
				} catch (error) {
					if (workflow.id === workflowId)
						message = error instanceof Error ? error.message : 'The action could not be updated.';
				}
			})
			.finally(() => {
				pendingCount -= 1;
				pending = pendingCount > 0;
			});
		operations = operationResult;
		return operationResult;
	}

	function updateInput(step: Step, field: string, input: Input): CanvasEffect {
		return effect(() => onInputChange(step.id, field, input));
	}

	function updateCondition(step: Step, update: ConditionUpdate): CanvasEffect {
		return effect(() => {
			const kind = workflowIndex.steps.get(step.id)?.kind;
			if (!kind || typeof kind === 'string') return;
			if (kind.If)
				return onEditStep(step.id, { If: { ...kind.If, condition: update(kind.If.condition) } });
			if (kind.While)
				return onEditStep(step.id, {
					While: { ...kind.While, condition: update(kind.While.condition) }
				});
		});
	}

	function changeVariableName(step: Step, event: Event) {
		if (
			!(event.currentTarget instanceof HTMLInputElement) ||
			typeof step.kind === 'string' ||
			!step.kind.SetVariable
		)
			return;
		const name = event.currentTarget.value;
		void effect(() => {
			const kind = workflowIndex.steps.get(step.id)?.kind;
			if (kind && typeof kind !== 'string' && kind.SetVariable)
				return onEditStep(step.id, { SetVariable: { ...kind.SetVariable, name } });
		});
	}

	function changeDuration(step: Step, event: Event) {
		if (!(event.currentTarget instanceof HTMLInputElement)) return;
		const millis = Math.round(event.currentTarget.valueAsNumber * 1000);
		if (!Number.isSafeInteger(millis) || millis < 0) {
			event.currentTarget.setCustomValidity('Enter a duration of zero or more seconds.');
			event.currentTarget.reportValidity();
			return;
		}
		event.currentTarget.setCustomValidity('');
		void effect(() => onEditStep(step.id, { Delay: { millis } }));
	}

	function startStepDrag(event: DragEvent, step: Step) {
		if (unavailable) {
			event.preventDefault();
			return;
		}
		onSelect(step.id);
		startWorkflowDrag(event, { type: 'step', workflowId: workflow.id, stepId: step.id });
		message = '';
	}

	function isValidDrag(
		destination: StepDestination,
		position: StepPosition,
		payload = currentWorkflowDrag()
	): boolean {
		if (unavailable || !payload) return false;
		if (payload.type === 'step')
			return (
				payload.workflowId === workflow.id &&
				validPlacement(workflow, destination, position, payload.stepId, workflowIndex)
			);
		return (
			catalog.some((item) => item.id === payload.catalogId) &&
			validPlacement(workflow, destination, position, undefined, workflowIndex)
		);
	}

	function dragOver(event: DragEvent, destination: StepDestination, position: StepPosition) {
		if (!isValidDrag(destination, position)) return;
		event.preventDefault();
		if (event.dataTransfer)
			event.dataTransfer.dropEffect = currentWorkflowDrag()?.type === 'catalog' ? 'copy' : 'move';
		dropTarget = placementKey(destination, position);
	}

	function finishDrag() {
		endWorkflowDrag();
		dropTarget = null;
		stopScroll();
	}

	function drop(event: DragEvent, destination: StepDestination, position: StepPosition) {
		event.preventDefault();
		event.stopPropagation();
		const payload = readWorkflowDrag(event);
		const accepted = isValidDrag(destination, position, payload);
		finishDrag();
		if (!accepted || !payload) return;
		if (payload.type === 'step') {
			void effect(() => onMove(payload.stepId, destination, position), payload.stepId);
		} else {
			const item = catalog.find((item) => item.id === payload.catalogId);
			if (item) void effect(() => onAdd(item.kind, destination, position));
		}
	}

	function scrollDuringDrag(event: DragEvent) {
		if (!event.dataTransfer?.types.includes(WORKFLOW_DRAG_TYPE)) return;
		const bounds = canvas.getBoundingClientRect();
		const edge = Math.min(64, bounds.height / 4);
		scrollSpeed =
			event.clientY < bounds.top + edge
				? -Math.ceil((bounds.top + edge - event.clientY) / 5)
				: event.clientY > bounds.bottom - edge
					? Math.ceil((event.clientY - bounds.bottom + edge) / 5)
					: 0;
		if (!scrollFrame && scrollSpeed) scrollFrame = requestAnimationFrame(scroll);
	}

	function scroll() {
		scrollFrame = 0;
		if (!scrollSpeed) return;
		canvas.scrollTop += Math.max(-20, Math.min(20, scrollSpeed));
		scrollFrame = requestAnimationFrame(scroll);
	}

	function stopScroll() {
		scrollSpeed = 0;
		if (scrollFrame) cancelAnimationFrame(scrollFrame);
		scrollFrame = 0;
	}

	function leaveCanvas(event: DragEvent) {
		if (event.relatedTarget instanceof Node && canvas.contains(event.relatedTarget)) return;
		dropTarget = null;
		stopScroll();
	}

	function openMove(step: Step, destination: StepDestination) {
		onSelect(step.id);
		moveStepId = moveStepId === step.id ? null : step.id;
		moveDestination = destination;
		movePosition = 'Append';
		insertTarget = null;
	}

	function confirmMove(step: Step) {
		const destination = moveDestination;
		const position = movePosition;
		if (unavailable || !validPlacement(workflow, destination, position, step.id, workflowIndex))
			return;
		void effect(() => onMove(step.id, destination, position), step.id);
	}

	function keyboardMove(
		event: KeyboardEvent,
		step: Step,
		list: readonly Step[],
		destination: StepDestination
	) {
		if (unavailable || !event.altKey || !['ArrowUp', 'ArrowDown'].includes(event.key)) return;
		event.preventDefault();
		const index = list.findIndex((sibling) => sibling.id === step.id);
		let position: StepPosition;
		if (event.key === 'ArrowUp') {
			if (index < 1) return;
			position = { Before: { step_id: list[index - 1].id } };
		} else {
			if (index === list.length - 1) return;
			position = list[index + 2] ? { Before: { step_id: list[index + 2].id } } : 'Append';
		}
		void effect(() => onMove(step.id, destination, position), step.id);
	}

	function backgroundClick(event: MouseEvent) {
		if (
			event.target === canvas ||
			(event.target instanceof HTMLElement && event.target.hasAttribute('data-canvas-background'))
		) {
			onSelect(null);
			moveStepId = null;
			insertTarget = null;
		}
	}

	function canvasKey(event: KeyboardEvent) {
		if (event.key !== 'Escape' || !(event.target instanceof Node) || !canvas.contains(event.target))
			return;
		event.stopPropagation();
		if (moveStepId || insertTarget) {
			moveStepId = null;
			insertTarget = null;
		} else onSelect(null);
		canvas.focus();
	}

	onDestroy(finishDrag);
</script>

{#snippet inputField(
	step: Step,
	id: string,
	label: string,
	input: Input,
	kind: 'text' | 'secret' | 'integer' | 'toggle' = 'text'
)}
	<InputControl
		{input}
		{label}
		{kind}
		steps={workflow.steps}
		index={workflowIndex}
		{schemas}
		{workflowTitles}
		{disabled}
		onChange={(value) => updateInput(step, id, value)}
		onSourcePicker={() => onSourcePicker(step.id, id, input)}
	/>
{/snippet}

{#snippet insertSlot(
	destination: StepDestination,
	position: StepPosition,
	connected: boolean,
	empty = false
)}
	{@const key = placementKey(destination, position)}
	<div
		class="insertion"
		class:connected
		class:empty
		class:active={dropTarget === key}
		data-insertion={key}
		ondragover={(event) => dragOver(event, destination, position)}
		ondrop={(event) => drop(event, destination, position)}
		role="group"
		aria-label="Insertion position"
	>
		<button
			class="insert-button"
			disabled={unavailable}
			aria-label={position === 'Append' ? 'Add action at end' : 'Add action before next action'}
			onclick={() => {
				insertTarget = insertTarget?.key === key ? null : { destination, position, key };
				moveStepId = null;
			}}
		>
			<Icon name="plus" size={14} />
			{#if empty}<span>Drop an action here</span>{/if}
		</button>
		{#if insertTarget?.key === key}
			<div class="insert-picker" role="group" aria-label="Choose action">
				{#if catalog.length === 0}<p>
						The action library is unavailable. Reload the workflow to try again.
					</p>{/if}
				{#each catalog as item (item.id)}
					<button
						disabled={unavailable}
						onclick={() => effect(() => onAdd(item.kind, destination, position))}
						>{item.title}</button
					>
				{/each}
			</div>
		{/if}
	</div>
{/snippet}

{#snippet moveControls(step: Step)}
	{#if moveStepId === step.id}
		<div class="move-controls" role="group" aria-label="Move action">
			<label
				>Move to
				<select
					aria-label="Move destination"
					disabled={unavailable}
					value={lists.findIndex((list) => sameDestination(list.destination, moveDestination))}
					onchange={(event) => {
						moveDestination = lists[Number(event.currentTarget.value)].destination;
						movePosition = 'Append';
					}}
				>
					{#each lists as list, index (index)}
						<option
							value={index}
							disabled={!validPlacement(
								workflow,
								list.destination,
								'Append',
								step.id,
								workflowIndex
							) && !sameDestination(list.destination, moveDestination)}
							>{index + 1}. {list.title}</option
						>
					{/each}
				</select>
			</label>
			<label
				>Position
				<select
					aria-label="Move position"
					disabled={unavailable}
					value={movePosition === 'Append' ? 'end' : movePosition.Before.step_id}
					onchange={(event) => {
						movePosition =
							event.currentTarget.value === 'end'
								? 'Append'
								: { Before: { step_id: event.currentTarget.value } };
					}}
				>
					<option value="end">At end</option>
					{#each moveChoices as target, index (target.id)}<option value={target.id}
							>Before {index + 1}. {stepTitle(target, schemas, workflowTitles)}</option
						>{/each}
				</select>
			</label>
			<button
				class="move-confirm"
				disabled={unavailable ||
					!validPlacement(workflow, moveDestination, movePosition, step.id, workflowIndex)}
				onclick={() => confirmMove(step)}>Move</button
			>
			<button
				class="quiet-button"
				disabled={unavailable}
				onclick={() => {
					moveStepId = null;
				}}>Cancel</button
			>
		</div>
	{/if}
{/snippet}

{#snippet stepHeader(step: Step, siblings: readonly Step[], destination: StepDestination)}
	<div class="step-header">
		<button
			class="drag-handle"
			aria-label={`Move ${stepTitle(step, schemas, workflowTitles)}`}
			title="Drag to move, or choose a position. Alt + Up / Down moves among nearby actions."
			draggable={!unavailable}
			disabled={unavailable}
			ondragstart={(event) => startStepDrag(event, step)}
			ondragend={finishDrag}
			onclick={() => openMove(step, destination)}
			onkeydown={(event) => keyboardMove(event, step, siblings, destination)}
			><Icon name="grip" size={16} /></button
		>
		<button
			class="step-title"
			data-step-title={step.id}
			aria-pressed={selectedStepId === step.id}
			onclick={() => onSelect(selectedStepId === step.id ? null : step.id)}
			onkeydown={(event) => keyboardMove(event, step, siblings, destination)}
		>
			<span class="action-icon"
				><Icon
					name={typeof step.kind === 'string'
						? 'power'
						: step.kind.If || step.kind.While || step.kind.OneOrMore
							? 'branch'
							: step.kind.Call
								? 'play'
								: step.kind.RequestInput
									? 'keyboard'
									: 'zap'}
					size={17}
				/></span
			>
			<span
				title={typeof step.kind !== 'string' && step.kind.OneOrMore
					? 'All children run; at least one must succeed.'
					: undefined}>{stepTitle(step, schemas, workflowTitles)}</span
			>
		</button>
	</div>
{/snippet}

{#snippet sequence(steps: readonly Step[], destination: StepDestination, nested = false)}
	<div class="sequence" class:nested data-canvas-background>
		{#each steps as step, index (step.id)}
			{@render insertSlot(destination, { Before: { step_id: step.id } }, index > 0)}
			{@const kind = step.kind}
			{@const isContainer =
				typeof kind !== 'string' &&
				(kind.If !== undefined || kind.While !== undefined || kind.OneOrMore !== undefined)}
			<section
				class="step"
				class:control={isContainer}
				class:selected={selectedStepId === step.id}
				data-step-id={step.id}
				aria-label={stepTitle(step, schemas, workflowTitles)}
			>
				{@render stepHeader(step, steps, destination)}
				{#if typeof kind !== 'string'}
					{#if kind.Action !== undefined}
						{@const schema = schemaFor(step, schemas)}
						<div class="fields">
							{#each actionSummaryFields(step, schemas, summaryFields) as id (id)}
								{@const input = kind.Action.inputs[id]}
								{@const field = schema?.fields.find((field) => field.id === id)}
								{#if input}{@render inputField(
										step,
										id,
										field?.label ?? fieldTitle(id),
										input,
										field?.kind ?? 'text'
									)}
								{:else}<button
										class="missing-field"
										{disabled}
										onclick={() => onSourcePicker(step.id, id, { Literal: null })}
										>Set {field?.label ?? fieldTitle(id)}</button
									>{/if}
							{/each}
						</div>
						{#if !schema}<span class="missing-schema">Saved action unavailable</span>{/if}
					{:else if kind.Delay !== undefined}
						<div class="duration">
							<label
								>Wait <input
									aria-label="Delay seconds"
									type="number"
									min="0"
									step="0.001"
									value={kind.Delay.millis / 1000}
									{disabled}
									onchange={(event) => changeDuration(step, event)}
								/> seconds</label
							>
						</div>
					{:else if kind.SetVariable !== undefined}
						{@const variable = kind.SetVariable}
						<div class="fields">
							<label class="name-field"
								>Name <input
									aria-label="Variable name"
									size={Math.max(4, Math.min(variable.name.length, 24))}
									value={variable.name}
									{disabled}
									onchange={(event) => changeVariableName(step, event)}
								/></label
							>
							{@render inputField(step, 'value', 'Value', variable.value)}
						</div>
					{:else if kind.If !== undefined || kind.While !== undefined}
						{@const condition = kind.If?.condition ?? kind.While?.condition}
						{#if condition}<div class="condition">
								<ConditionControl
									{condition}
									steps={workflow.steps}
									index={workflowIndex}
									{schemas}
									{workflowTitles}
									{disabled}
									onChange={(condition) => updateCondition(step, condition)}
									onSourcePicker={(path, input) => onSourcePicker(step.id, path, input)}
								/>
							</div>{/if}
					{:else if kind.RequestInput !== undefined}
						<div class="fields">
							{#each kind.RequestInput.fields.slice(0, 1) as field (field.id)}
								{@render inputField(
									step,
									`fields.${field.id}.default`,
									`${field.label || fieldTitle(field.id)} default`,
									field.default ?? { Literal: '' }
								)}
							{/each}
						</div>
					{:else if kind.Call !== undefined && typeof kind.Call.binding !== 'string'}
						<div class="fields">
							{#each Object.entries(kind.Call.binding.Explicit.inputs).slice(0, 1) as [id, input] (id)}{@render inputField(
									step,
									id,
									fieldTitle(id),
									input
								)}{/each}
						</div>
					{/if}
				{/if}
				{#if onInspectStep}<button
						class="details-button"
						aria-label={`Details for ${stepTitle(step, schemas, workflowTitles)}`}
						{disabled}
						onclick={() => {
							onSelect(step.id);
							onInspectStep?.(step.id);
						}}><Icon name="pencil" size={14} /></button
					>{/if}
				{@render moveControls(step)}
			</section>
			{#if typeof kind !== 'string'}
				{#if kind.If !== undefined}
					{@render sequence(
						kind.If.then_steps,
						{ Branch: { parent_id: step.id, branch: 'Then' } },
						true
					)}
					<div class="branch-row"><Icon name="branch" size={15} /><span>Otherwise</span></div>
					{@render sequence(
						kind.If.else_steps,
						{ Branch: { parent_id: step.id, branch: 'Else' } },
						true
					)}
					<div class="branch-row end-row">End If</div>
				{:else if kind.While !== undefined}
					{@render sequence(
						kind.While.steps,
						{ Branch: { parent_id: step.id, branch: 'Body' } },
						true
					)}
					<div class="branch-row end-row">End While</div>
				{:else if kind.OneOrMore !== undefined}
					{@render sequence(
						kind.OneOrMore.steps,
						{ Branch: { parent_id: step.id, branch: 'Body' } },
						true
					)}
					<div class="branch-row end-row">End One or More</div>
				{/if}
			{/if}
		{/each}
		{@render insertSlot(destination, 'Append', steps.length > 0, steps.length === 0)}
	</div>
{/snippet}

<svelte:window onkeydown={canvasKey} />

<div
	class="workflow-editor canvas"
	bind:this={canvas}
	role="region"
	aria-label="Workflow canvas"
	aria-busy={pending}
	tabindex="-1"
	onpointerdown={backgroundClick}
	ondragover={scrollDuringDrag}
	ondragleave={leaveCanvas}
	ondrop={finishDrag}
	ondragend={finishDrag}
>
	<div class="canvas-content" data-canvas-background>
		<div class="keyboard-hint">
			<span>Drag actions to reorder. Use a move handle to choose a position.</span
			>{#if selectedStepId}<button class="quiet-button" onclick={() => onSelect(null)}
					>Deselect action</button
				>{/if}
		</div>
		{#if message}<p class="operation-message" role="alert">{message}</p>{/if}
		{@render sequence(workflow.steps, 'Root')}
		{#if workflow.steps.length === 0}<p class="empty-message" data-canvas-background>
				Add your first action from the action library.
			</p>{/if}
		<span class="status" role="status">{pending ? 'Saving action…' : ''}</span>
	</div>
</div>

<style>
	.canvas {
		background: var(--ink);
		height: 100%;
		min-height: 280px;
		overflow: auto;
		overscroll-behavior: contain;
		position: relative;
	}
	.canvas-content {
		width: min(100%, 740px);
		margin: 0 auto;
		padding: 24px 28px 80px;
		min-height: 100%;
	}
	.keyboard-hint {
		margin: 0 0 18px;
		font-size: 12px;
		color: var(--muted);
	}
	.sequence {
		min-width: 0;
	}
	.sequence.nested {
		margin-left: 34px;
		padding: 2px 0;
	}
	.step {
		background: var(--surface);
		border: 1px solid var(--border);
		border-radius: 10px;
		padding: 10px 12px;
		display: flex;
		align-items: center;
		gap: 8px 12px;
		flex-wrap: wrap;
		box-shadow: 0 2px 4px #0003;
		min-width: 0;
	}
	.step.selected {
		border-color: var(--gold);
		box-shadow: 0 0 0 1px var(--gold);
	}
	.step.control {
		background: var(--side);
		padding: 7px 12px;
	}
	.step-header {
		display: flex;
		align-items: center;
		gap: 8px;
		flex: none;
		max-width: 100%;
	}
	.step-title {
		display: flex;
		align-items: center;
		gap: 10px;
		font-weight: 600;
		text-align: left;
		flex: 1;
		min-width: 0;
		border: 0;
		background: none;
		padding: 0;
		min-height: 30px;
	}
	.step-title > span:last-child {
		overflow-wrap: anywhere;
	}
	.action-icon {
		width: 30px;
		height: 30px;
		border-radius: 7px;
		background: var(--maroon);
		display: grid;
		place-items: center;
		flex: none;
	}
	.control .action-icon {
		background: var(--surface);
		color: var(--amber);
	}
	.drag-handle {
		display: grid;
		place-items: center;
		background: none;
		border: 0;
		width: 20px;
		padding: 4px 0;
		color: var(--muted);
		cursor: grab;
	}
	.drag-handle:active {
		cursor: grabbing;
	}
	.fields {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		flex: 1;
		min-width: 0;
		gap: 8px 12px;
	}
	.fields > :global(.input-control) {
		flex: 0 1 auto;
		min-width: 100px;
	}
	.missing-schema {
		grid-column: 1 / -1;
		color: var(--muted);
		font-size: 12px;
		margin: 0;
	}
	.fields:empty {
		display: none;
	}
	.details-button {
		background: none;
		border: 0;
		border-radius: 5px;
		padding: 5px;
		color: var(--muted);
		display: grid;
		place-items: center;
		margin-left: auto;
	}
	.condition {
		flex: 1;
		min-width: 180px;
	}
	.duration {
		margin: 0;
		color: var(--muted);
	}
	.duration input {
		width: 100px;
		margin: 0 6px;
	}
	.duration input,
	.name-field input,
	.move-controls select {
		background: var(--field);
		border: 1px solid var(--border);
		border-radius: 6px;
		padding: 6px 9px;
		min-width: 0;
	}
	.name-field {
		display: flex;
		align-items: center;
		gap: 8px;
		color: var(--muted);
		font-size: 12px;
	}
	.name-field input {
		min-height: 32px;
		font-size: 14px;
	}
	.branch-row {
		background: var(--side);
		border: 1px solid var(--border);
		border-radius: 8px;
		min-height: 38px;
		padding: 8px 16px 8px 40px;
		display: flex;
		gap: 10px;
		align-items: center;
		color: var(--muted);
	}
	.end-row {
		min-height: 34px;
	}
	.insertion {
		position: relative;
		height: 12px;
		display: flex;
		align-items: center;
		justify-content: center;
	}
	.insertion.connected::before {
		content: '';
		position: absolute;
		top: 0;
		bottom: 0;
		width: 2px;
		background: var(--border);
		left: calc(50% - 1px);
	}
	.insertion.active::after {
		content: '';
		position: absolute;
		inset: 5px 0;
		background: var(--gold);
		border-radius: 2px;
		pointer-events: none;
	}
	.insert-button {
		position: relative;
		z-index: 1;
		border: 1px solid transparent;
		border-radius: 5px;
		background: var(--ink);
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 8px;
		padding: 0 8px;
		min-height: 18px;
		color: var(--muted);
		opacity: 0;
	}
	.insertion:hover .insert-button,
	.insert-button:focus-visible,
	.insertion.active .insert-button {
		opacity: 1;
	}
	.insertion.empty {
		height: 52px;
		margin: 10px 0;
		border: 1px dashed var(--border);
		border-radius: 8px;
	}
	.insertion.empty .insert-button {
		opacity: 1;
		padding: 6px 12px;
	}
	.insertion.active .insert-button {
		color: var(--gold);
		border-color: var(--gold);
	}
	.insert-picker {
		position: absolute;
		top: 14px;
		left: 50%;
		transform: translateX(-50%);
		z-index: 2;
		padding: 6px;
		width: min(280px, 100%);
		background: var(--side);
		border: 1px solid var(--border);
		border-radius: 8px;
		box-shadow: 0 6px 20px #0008;
		max-height: 260px;
		overflow: auto;
	}
	.insert-picker button {
		display: block;
		width: 100%;
		border: 0;
		background: none;
		text-align: left;
		border-radius: 5px;
		padding: 8px;
	}
	.insert-picker button:hover {
		background: var(--surface);
	}
	.move-controls {
		width: 100%;
		display: flex;
		gap: 8px;
		flex-wrap: wrap;
		margin: 12px 0 0 28px;
		align-items: end;
	}
	.move-controls label {
		display: flex;
		flex: 1;
		min-width: 140px;
		align-items: center;
		gap: 8px;
		color: var(--muted);
		font-size: 12px;
	}
	.move-controls select {
		width: 100%;
		font-size: 14px;
	}
	.move-confirm,
	.quiet-button,
	.missing-field {
		border: 1px solid var(--border);
		border-radius: 6px;
		padding: 6px 10px;
		background: var(--side);
		min-height: 32px;
	}
	.move-confirm {
		background: var(--blood);
		border-color: var(--blood);
	}
	.missing-field {
		text-align: left;
		color: var(--muted);
	}
	.empty-message {
		color: var(--muted);
		text-align: center;
		margin-top: 16px;
	}
	.operation-message {
		background: var(--side);
		border-left: 3px solid var(--amber);
		padding: 10px 12px;
		margin: 0 0 14px;
	}
	.status {
		position: absolute;
		width: 1px;
		height: 1px;
		overflow: hidden;
		clip-path: inset(50%);
	}
	@media (max-width: 520px) {
		.canvas-content {
			padding: 16px 12px 60px;
		}
		.fields {
			grid-template-columns: 1fr;
		}
		.sequence.nested {
			margin-left: 20px;
		}
	}
</style>
