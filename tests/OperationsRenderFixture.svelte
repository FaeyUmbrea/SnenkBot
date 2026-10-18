<script lang="ts">
	import DesktopShell from '../src/lib/DesktopShell.svelte';
	import HistoryView from '../src/lib/HistoryView.svelte';
	import WorkflowInputDialog from '../src/lib/WorkflowInputDialog.svelte';
	import type { InputRequest, RunRecord_Serialize } from '../src/lib/contracts/index';
	import { schemas } from './fixture';
	const query = new URLSearchParams(location.search);
	const prompt = query.get('scenario') === 'prompt';
	const mode = query.get('mode');
	let requestedRunId = $state<string | null>(mode === 'detail' ? 'fixture-run-0' : null);
	let request = $state<InputRequest | null>(
		prompt
			? {
					request_id: 'fixture-prompt',
					title: 'Edit stream details',
					fields: [
						{ id: 'title', label: 'Stream title', required: true, default: null },
						{ id: 'game', label: 'Game', required: true, default: null }
					],
					defaults: {},
					values: { title: 'Sunday stories with Faey', game: 'Guild Wars 2' },
					validating: mode === 'validating',
					error:
						mode === 'error'
							? 'This game could not be found. Choose a game available on Twitch and try again. '.repeat(
									20
								)
							: null
				}
			: null
	);
	const records: RunRecord_Serialize[] = Array.from({ length: 5 }, (_, index) => ({
		run_id: `fixture-run-${index}`,
		workflow_id: `workflow-${index}`,
		workflow_revision: 2,
		trigger: { kind: index === 1 ? 'manual' : 'event', id: null },
		started_at_ms: 1_790_000_000_000 - index * 60000,
		finished_at_ms: 1_790_000_000_750 - index * 60000,
		outcome: index === 0 ? { status: 'failed', category: 'connector' } : { status: 'succeeded' },
		step_trace: [
			{
				sequence: 1,
				parent_sequence: null,
				workflow_id: `workflow-${index}`,
				workflow_revision: 2,
				step_id: 'fixture-title',
				kind: { kind: 'action', capability: schemas[0].id, version: schemas[0].version },
				started_at_ms: 1_790_000_000_000,
				finished_at_ms: 1_790_000_000_750,
				duration_ms: 750,
				outcome:
					index === 0
						? {
								status: 'failed',
								kind: 'connector_unavailable',
								remote_effect_uncertain: false,
								continued_by_policy: false
							}
						: { status: 'succeeded' }
			}
		]
	}));
	const workflowTitles = {
		'workflow-0': 'Update stream details',
		'workflow-1': 'Sunday stream',
		'workflow-2': 'Recording chapters',
		'workflow-3': 'Shoutout',
		'workflow-4': 'A little welcome'
	};
	const noEffect = () => {};
</script>

<DesktopShell
	page={prompt ? 'automations' : 'history'}
	title={prompt ? 'Sunday stream' : 'Run History'}
	connections={[]}
	onNavigate={noEffect}
	onConfigure={noEffect}
	onDismissError={noEffect}
>
	<HistoryView
		{requestedRunId}
		onSelectRun={(runId) => {
			requestedRunId = runId;
		}}
		{schemas}
		{workflowTitles}
		loadPage={async (query) => ({
			entries: records
				.filter(
					(record) => !query.filter?.outcome || record.outcome.status === query.filter.outcome
				)
				.map((summary) => ({ kind: 'record' as const, summary })),
			next_cursor: null
		})}
		inspectRun={async (query) => records.find((record) => record.run_id === query.run_id)!}
	/>
	<WorkflowInputDialog
		{request}
		onSubmit={async () => {
			if (request) request = { ...request, validating: true };
		}}
		onCancel={async () => {
			request = null;
		}}
	/>
</DesktopShell>

<style>
	:global(body) {
		margin: 0;
		height: 100vh;
		background: #0b0808;
	}
	:global(#fixture) {
		height: 100%;
	}
</style>
