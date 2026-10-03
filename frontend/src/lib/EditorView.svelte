<script lang="ts">
	import { untrack } from 'svelte';
	import type { createApplicationController } from './application';
	import type {
		Input,
		StepKind,
		StepDestination,
		StepPosition,
		ValueSource
	} from './contracts/index';
	import type { CatalogItem } from './canvas';
	import { allSteps, schemaFor, stepTitle } from './canvas';
	import WorkflowCanvas from './WorkflowCanvas.svelte';
	import ActionCatalog from './ActionCatalog.svelte';
	import InputControl from './InputControl.svelte';
	let { controller }: { controller: ReturnType<typeof createApplicationController> } = $props();
	const application = untrack(() => controller);
	const resources = untrack(() => controller.resources);
	let tab = $state<'actions' | 'details'>('actions');
	let picker = $state<{ stepId: string; fieldId: string; current: Input } | null>(null);
	let sourceQuery = $state('');
	const editor = $derived($application.editor!);
	const workflow = $derived(editor.draft.workflow);
	const steps = $derived(allSteps(workflow.steps));
	const selected = $derived(steps.find((s) => s.id === $application.selectedStepId));
	const selectedKind = $derived(
		selected && typeof selected.kind !== 'string' ? selected.kind : null
	);
	const selectedSchema = $derived(selected ? schemaFor(selected, $application.schemas) : undefined);
	const titles = $derived(
		Object.fromEntries(($application.snapshot?.workflows ?? []).map((w) => [w.id, w.title]))
	);
	const catalog = $derived<CatalogItem[]>(
		$application.definitions.map((d) => ({
			id: d.schema.id,
			title: d.schema.title,
			category: d.schema.title.includes('OBS') ? 'Broadcast' : 'Actions',
			kind: {
				Action: {
					capability: d.schema.id,
					version: d.schema.version,
					inputs: Object.fromEntries(
						d.schema.fields.map((f) => [
							f.id,
							{ Literal: Object.hasOwn(d.defaults, f.id) ? d.defaults[f.id] : null }
						])
					),
					deadline_ms: null
				}
			}
		}))
	);
	const values = $derived(
		$resources.values
			.filter((v) => `${v.label} ${v.detail}`.toLowerCase().includes(sourceQuery.toLowerCase()))
			.slice(0, 50)
	);
	function quiet(p: Promise<unknown>) {
		void p.catch(() => undefined);
	}
	async function edit(kind: Parameters<typeof controller.edit>[0]) {
		await controller.edit(kind);
	}
	async function add(
		kind: StepKind,
		destination: StepDestination = 'Root',
		position: StepPosition = 'Append'
	) {
		await edit({ operation: 'insert_at', kind, destination, position });
	}
	function source(stepId: string, fieldId: string, current: Input) {
		controller.selectStep(stepId);
		picker = { stepId, fieldId, current };
		sourceQuery = '';
		quiet(resources.loadValues());
	}
	async function choose(value: ValueSource) {
		if (!picker) return;
		const fallback = value.optional ? { Literal: null } : null;
		let input: Input;
		if (value.source === 'step')
			input = { Reference: { step_id: value.source_id, output_id: value.output_id, fallback } };
		else if (value.source === 'variable') input = { Variable: { name: value.source_id, fallback } };
		else input = { Trigger: { name: value.source_id, fallback } };
		await edit({
			operation: 'set_input',
			step_id: picker.stepId,
			field_id: picker.fieldId,
			value: input
		});
		picker = null;
	}
	function triggerTitle(kind: (typeof editor.draft.triggers)[number]['kind']) {
		if (kind.kind === 'manual') return 'Manual run';
		if (kind.kind === 'obs.recording_started') return 'Recording starts';
		if (kind.kind === 'obs.current_scene') return 'OBS scene changes';
		const labels: Record<string, string> = {
			'chat.command': 'Twitch chat command',
			'channel.details_changed': 'Stream title or game changes',
			'ad_break.begin': 'Twitch ad break starts',
			'recording.started': 'Recording starts'
		};
		return labels[kind.event] ?? 'Integration event';
	}
</script>

<div class="workflow-editor editor-layout">
	<section class="flow">
		<div class="edit-toolbar">
			<label class="name-field"
				>Automation name <input
					aria-label="Automation name"
					value={editor.draft.name ?? ''}
					onchange={(e) => quiet(edit({ operation: 'rename', name: e.currentTarget.value }))}
				/></label
			><label
				><input
					type="checkbox"
					checked={editor.draft.enabled}
					onchange={(e) => quiet(edit({ operation: 'enabled', enabled: e.currentTarget.checked }))}
				/>Enabled</label
			><button disabled={!editor.can_undo} onclick={() => quiet(edit({ operation: 'undo' }))}
				>Undo</button
			><button disabled={!editor.can_redo} onclick={() => quiet(edit({ operation: 'redo' }))}
				>Redo</button
			><button disabled={!editor.dirty} onclick={() => quiet(controller.saveEditor())}>Save</button
			><button
				disabled={!editor.draft.enabled || editor.dirty}
				onclick={() => quiet(controller.run(workflow.id))}>Run</button
			><span>{editor.dirty ? 'Unsaved changes' : `Saved revision ${editor.saved_revision}`}</span>
		</div>
		<section class="triggers" aria-label="Triggers">
			<h2>Triggers</h2>
			{#each editor.draft.triggers as trigger (trigger.id)}<div class="trigger">
					<strong>{triggerTitle(trigger.kind)}</strong><label
						><input
							type="checkbox"
							checked={trigger.enabled}
							onchange={(e) =>
								quiet(
									edit({
										operation: 'trigger_enabled',
										trigger_id: trigger.id,
										enabled: e.currentTarget.checked
									})
								)}
						/>Enabled</label
					>{#if trigger.kind.kind === 'integration_event'}{#each Object.entries(trigger.kind.filters ?? {}) as [key, value] (key)}<label
								>{key === 'command'
									? 'Command'
									: key === 'aliases'
										? 'Aliases'
										: key === 'permission'
											? 'Who can use this'
											: key.replaceAll('_', ' ')}<input
									value={typeof value === 'string' ? value : JSON.stringify(value)}
									onchange={(e) => {
										if (trigger.kind.kind !== 'integration_event') return;
										let next: unknown = e.currentTarget.value;
										if (typeof value !== 'string') {
											try {
												next = JSON.parse(e.currentTarget.value);
											} catch {
												return;
											}
										}
										quiet(
											edit({
												operation: 'trigger_kind',
												trigger_id: trigger.id,
												kind: { ...trigger.kind, filters: { ...trigger.kind.filters, [key]: next } }
											})
										);
									}}
								/></label
							>{/each}{/if}<button
						aria-label={`Remove ${triggerTitle(trigger.kind)}`}
						onclick={() => quiet(edit({ operation: 'remove_trigger', trigger_id: trigger.id }))}
						>Remove</button
					>
				</div>{/each}
			<div class="trigger-actions">
				<button
					onclick={() => quiet(edit({ operation: 'append_trigger', kind: { kind: 'manual' } }))}
					>Add manual trigger</button
				><button
					onclick={() =>
						quiet(edit({ operation: 'append_trigger', kind: { kind: 'obs.recording_started' } }))}
					>Add recording trigger</button
				>
			</div>
		</section>
		<WorkflowCanvas
			{workflow}
			schemas={$application.schemas}
			workflowTitles={titles}
			{catalog}
			selectedStepId={$application.selectedStepId}
			onSelect={controller.selectStep}
			onMove={(step_id, destination, position) =>
				edit({ operation: 'move_to', step_id, destination, position })}
			onAdd={add}
			onInputChange={(step_id, field_id, value) =>
				edit({ operation: 'set_input', step_id, field_id, value })}
			onEditStep={(step_id, kind) => edit({ operation: 'set_kind', step_id, kind })}
			onSourcePicker={source}
			onInspectStep={(id) => {
				controller.selectStep(id);
				tab = 'details';
			}}
		/>
	</section>
	<aside class="inspector">
		<div class="tabs">
			<button aria-pressed={tab === 'actions'} onclick={() => (tab = 'actions')}>Actions</button
			><button aria-pressed={tab === 'details'} onclick={() => (tab = 'details')}>Details</button>
		</div>
		{#if picker}<section class="details">
				<h2>Choose a value</h2>
				<input
					type="search"
					aria-label="Find a value"
					bind:value={sourceQuery}
				/>{#if $resources.valuesError}<p role="alert">{$resources.valuesError}</p>
					<button onclick={() => quiet(resources.loadValues())}>Retry</button
					>{/if}{#if $resources.valuesLoading}<p>
						Loading values…
					</p>{/if}{#each values as value (`${value.source}:${value.source_id}:${value.output_id}`)}<button
						class="source"
						onclick={() => quiet(choose(value))}
						><strong>{value.label}</strong><span>{value.detail}</span></button
					>{/each}<button
					onclick={() => {
						if (picker)
							quiet(
								edit({
									operation: 'set_input',
									step_id: picker.stepId,
									field_id: picker.fieldId,
									value: { Literal: '' }
								})
							);
						picker = null;
					}}>Use text</button
				><button onclick={() => (picker = null)}>Cancel</button>
			</section>
		{:else if tab === 'actions'}<ActionCatalog
				items={catalog}
				onChoose={(item) => quiet(add(item.kind))}
			/>
			<div class="flow-actions">
				<h2>Flow</h2>
				<button
					onclick={() =>
						quiet(
							add({
								If: {
									condition: { Equal: [{ Literal: true }, { Literal: true }] },
									then_steps: [],
									else_steps: []
								}
							})
						)}>If</button
				><button onclick={() => quiet(add({ Delay: { millis: 1000 } }))}>Delay</button><button
					onclick={() => quiet(add({ SetVariable: { name: 'value', value: { Literal: '' } } }))}
					>Set variable</button
				><button onclick={() => quiet(add('Stop'))}>Stop this automation</button>
			</div>
		{:else if selected}<section class="details">
				<h2>{stepTitle(selected, $application.schemas, titles)}</h2>
				{#if selectedKind?.Action}{#each selectedSchema?.fields ?? [] as field, index (field.id)}<div
							class="field"
							class:last-field={index === (selectedSchema?.fields.length ?? 0) - 1}
						>
							<InputControl
								input={selectedKind?.Action.inputs[field.id] ?? { Literal: null }}
								label={field.label}
								kind={field.kind}
								{steps}
								schemas={$application.schemas}
								workflowTitles={titles}
								onChange={(value) =>
									edit({ operation: 'set_input', step_id: selected.id, field_id: field.id, value })}
								onSourcePicker={() =>
									source(
										selected.id,
										field.id,
										selectedKind?.Action?.inputs[field.id] ?? { Literal: null }
									)}
							/>
							{#if field.description}<p>{field.description}</p>{/if}
							{#if field.choice_source}<button
									onclick={() =>
										quiet(
											resources.loadChoices({
												action_id: selectedKind!.Action!.capability,
												field_id: field.id,
												depends_on: field.choice_source!.depends_on
													? String(
															selectedKind!.Action!.inputs[field.choice_source!.depends_on]
																?.Literal ?? ''
														)
													: null
											})
										)}>Choose {field.label.toLowerCase()}</button
								>{#if $resources.choiceRequest?.field_id === field.id}{#each $resources.choices as choice (choice.value)}<button
											onclick={() =>
												quiet(
													edit({
														operation: 'set_input',
														step_id: selected.id,
														field_id: field.id,
														value: { Literal: choice.value }
													})
												)}>{choice.label}</button
										>{/each}{/if}{/if}
						</div>{/each}{:else if selectedKind?.Call}<label
						>Automation<select
							value={selectedKind?.Call.workflow_id}
							onchange={(e) =>
								quiet(
									edit({
										operation: 'set_kind',
										step_id: selected.id,
										kind: {
											Call: {
												workflow_id: e.currentTarget.value,
												binding: selectedKind?.Call!.binding
											}
										}
									})
								)}
							>{#each $application.snapshot?.workflows ?? [] as item (item.id)}<option
									value={item.id}>{item.title}</option
								>{/each}</select
						></label
					>{:else if selectedKind?.SetVariable}<label
						>Variable name<input
							value={selectedKind?.SetVariable.name}
							onchange={(e) =>
								quiet(
									edit({
										operation: 'set_kind',
										step_id: selected.id,
										kind: {
											SetVariable: { ...selectedKind!.SetVariable!, name: e.currentTarget.value }
										}
									})
								)}
						/></label
					>{/if}<button onclick={() => quiet(edit({ operation: 'remove', step_id: selected.id }))}
					>Remove action</button
				><button onclick={() => controller.selectStep(null)}>Close details</button>
			</section>{:else}<section class="details">
				<h2>Automation details</h2>
				<p>Select an action to edit its fields.</p>
			</section>{/if}
	</aside>
</div>

<style>
	.editor-layout {
		display: grid;
		grid-template-columns: minmax(0, 1fr) 384px;
		height: 100%;
		min-height: 0;
		overflow: hidden;
	}
	.flow {
		overflow: auto;
		min-width: 0;
	}
	.inspector {
		overflow: auto;
		background: var(--side);
		border-left: 1px solid var(--border);
	}
	.edit-toolbar {
		display: flex;
		align-items: center;
		gap: 8px;
		flex-wrap: wrap;
		padding: 16px 24px;
		border-bottom: 1px solid var(--border);
	}
	.edit-toolbar label {
		display: flex;
		gap: 6px;
		align-items: center;
	}
	.name-field {
		min-width: 0;
		flex-wrap: wrap;
	}
	.name-field input {
		width: 212px;
	}
	.edit-toolbar span {
		font-size: 12px;
		color: var(--muted);
	}
	.triggers {
		padding: 24px;
	}
	.triggers h2,
	.flow-actions h2 {
		font-size: 14px;
		margin: 0 0 12px;
	}
	.trigger-actions {
		display: flex;
		flex-wrap: wrap;
		gap: 8px;
	}
	.trigger {
		background: var(--surface);
		padding: 12px;
		margin: 8px 0;
		display: flex;
		gap: 10px;
		align-items: center;
		flex-wrap: wrap;
		border-radius: 8px;
	}
	.trigger label {
		min-width: 0;
		max-width: 100%;
		flex-wrap: wrap;
		display: flex;
		gap: 6px;
		align-items: center;
	}
	.tabs {
		display: flex;
		justify-content: center;
		gap: 8px;
		padding: 16px;
	}
	.details,
	.flow-actions {
		padding: 24px;
		display: flex;
		flex-direction: column;
		gap: 12px;
	}
	.details h2 {
		margin: 0;
		font-size: 16px;
	}
	.field {
		padding-bottom: 16px;
		border-bottom: 1px solid var(--border);
	}
	.field.last-field {
		border-bottom: 0;
		padding-bottom: 0;
	}
	.field p {
		font-size: 12px;
		color: var(--muted);
	}
	.source {
		display: flex;
		flex-direction: column;
		gap: 4px;
		text-align: left;
	}
	.source span {
		font-size: 12px;
		color: var(--muted);
	}
	input,
	select {
		min-width: 0;
		max-width: 100%;
		min-height: 32px;
		background: var(--field);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 5px;
		color: inherit;
	}
	button {
		background: var(--surface);
		border: 1px solid var(--border);
		border-radius: 6px;
		padding: 7px 10px;
		color: inherit;
		cursor: pointer;
	}
	button:disabled {
		opacity: 0.5;
		cursor: default;
	}
	@media (max-width: 1150px) {
		.editor-layout {
			grid-template-columns: minmax(0, 1fr) 320px;
		}
	}
</style>
