<script lang="ts">
	import WorkflowCanvas from '../src/lib/WorkflowCanvas.svelte';
	import ActionCatalog from '../src/lib/ActionCatalog.svelte';
	import { catalog, schemas, step, workflow as fixtureWorkflow, workflowTitles } from './fixture';
	import type { Workflow } from '../src/lib/contracts/index';

	let selectedStepId = $state<string | null>(null);
	const scenario = new URLSearchParams(location.search).get('scenario');
	const workflow: Workflow = structuredClone(fixtureWorkflow);
	if (scenario === 'long') {
		workflow.steps.unshift(
			step('long', {
				SetVariable: {
					name: 'Name is a neutral editable field, even when this label is long',
					value: {
						Literal:
							'A longer value remains readable and editable in an ordinary field without being cut down to three letters.'
					}
				}
			})
		);
	}
	const noEffect = () => {};
</script>

<div class="render-shell">
	<header><strong>SnenkBot</strong><span>Sunday stream</span></header>
	<div class="render-editor">
		<ActionCatalog items={catalog} onChoose={noEffect} />
		<WorkflowCanvas
			{workflow}
			{schemas}
			{workflowTitles}
			{catalog}
			{selectedStepId}
			onSelect={(id) => {
				selectedStepId = id;
			}}
			onMove={noEffect}
			onAdd={noEffect}
			onInputChange={noEffect}
			onEditStep={noEffect}
			onSourcePicker={noEffect}
		/>
	</div>
</div>

<style>
	:global(body) {
		margin: 0;
		background: #0b0808;
	}
	.render-shell {
		font-family: 'Inter Tight', sans-serif;
		color: #e8e2d9;
		height: 100vh;
		display: flex;
		flex-direction: column;
	}
	header {
		height: 56px;
		flex: none;
		display: flex;
		align-items: center;
		gap: 32px;
		padding: 0 24px;
		background: #1a1413;
		border-bottom: 1px solid #3b2f2a;
	}
	header strong {
		color: #e6c655;
	}
	.render-editor {
		display: grid;
		grid-template-columns: 248px 1fr;
		flex: 1;
		min-height: 0;
	}
	@media (max-width: 720px) {
		.render-editor {
			grid-template-columns: 190px 1fr;
		}
	}
</style>
