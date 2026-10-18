<script lang="ts">
	import DesktopShell from '../src/lib/DesktopShell.svelte';
	import HomeView from '../src/lib/HomeView.svelte';
	import type { ConnectionStatus, HistoryPage, WorkflowStatus } from '../src/lib/contracts/index';
	const connections: ConnectionStatus[] = [
		{
			integration: 'obs',
			connection: 'main',
			title: 'OBS Studio',
			state: 'connected',
			detail: 'Ready for recording'
		},
		{
			integration: 'twitch',
			connection: 'broadcaster',
			title: 'Twitch broadcaster',
			state: 'connected',
			detail: 'Chat and events connected'
		},
		{
			integration: 'vtube_studio',
			connection: 'main',
			title: 'VTube Studio',
			state: 'connecting',
			detail: 'Connecting…'
		}
	];
	const workflows: WorkflowStatus[] = [
		{
			id: 'welcome',
			title: 'A little welcome',
			enabled: true,
			trigger_summary: 'Twitch chat · !hello',
			step_count: 3,
			category: 'Chat',
			capabilities: [],
			capability_titles: {},
			integration_usage: {},
			revision: 1,
			has_steps: true,
			error: null
		}
	];
	workflows.push(
		{
			...workflows[0],
			id: 'chapters',
			title: 'Recording chapters',
			trigger_summary: 'OBS recording started',
			category: 'Broadcast',
			step_count: 4
		},
		{ ...workflows[0], id: 'shoutout', title: 'Shoutout', trigger_summary: '!so', step_count: 3 }
	);
	const history: HistoryPage = {
		entries: workflows.map((workflow, index) => ({
			kind: 'record',
			summary: {
				run_id: `run-${index}`,
				workflow_id: workflow.id,
				workflow_revision: 1,
				trigger: { kind: 'manual', id: null },
				started_at_ms: 1791000000000 - index * 60000,
				finished_at_ms: 1791000001000 - index * 60000,
				outcome: { status: 'succeeded' }
			}
		})),
		next_cursor: null
	};
	const noEffect = () => {};
</script>

<DesktopShell
	page="home"
	title="Home"
	{connections}
	onNavigate={noEffect}
	onConfigure={noEffect}
	onDismissError={noEffect}
>
	<HomeView
		{connections}
		{workflows}
		{history}
		onConfigure={noEffect}
		onNavigate={noEffect}
		onOpen={noEffect}
		onRun={async () => {}}
		onLibrary={noEffect}
		onHistory={noEffect}
		onRefreshHistory={noEffect}
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
