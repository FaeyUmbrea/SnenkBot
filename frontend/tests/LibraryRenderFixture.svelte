<script lang="ts">
	import DesktopShell from '../src/lib/DesktopShell.svelte';
	import WorkflowLibraryView from '../src/lib/WorkflowLibraryView.svelte';
	import type { ConnectionStatus, WorkflowStatus } from '../src/lib/contracts/index';
	const connections: ConnectionStatus[] = [];
	const workflows: WorkflowStatus[] = [
		{
			id: 'starting-stream',
			title: 'Starting the stream',
			enabled: true,
			trigger_summary: 'Manual +1',
			step_count: 4,
			category: 'Broadcast',
			capabilities: [],
			capability_titles: {},
			integration_usage: {},
			revision: 3,
			has_steps: true,
			error: null
		},
		{
			id: 'welcome-chat',
			title: 'Welcome message',
			enabled: true,
			trigger_summary: 'Twitch chat message',
			step_count: 2,
			category: 'Chat',
			capabilities: [],
			capability_titles: {},
			integration_usage: {},
			revision: 2,
			has_steps: true,
			error: null
		},
		{
			id: 'empty-workflow',
			title: 'New workflow',
			enabled: false,
			trigger_summary: 'Manual run',
			step_count: 0,
			category: 'Automation',
			capabilities: [],
			capability_titles: {},
			integration_usage: {},
			revision: 1,
			has_steps: false,
			error: null
		},
		{
			id: 'unavailable-workflow',
			title: 'Unavailable workflow',
			enabled: true,
			trigger_summary: 'OBS program scene changed',
			step_count: 1,
			category: 'Broadcast',
			capabilities: [],
			capability_titles: {},
			integration_usage: {},
			revision: null,
			has_steps: true,
			error: 'Could not load workflow.'
		}
	];
	const noEffect = () => {};
</script>

<DesktopShell
	page="automations"
	title="Automations"
	{connections}
	onNavigate={noEffect}
	onConfigure={noEffect}
	onDismissError={noEffect}
>
	<WorkflowLibraryView
		{workflows}
		onOpen={noEffect}
		onRun={async () => {}}
		onCreate={async () => {}}
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
