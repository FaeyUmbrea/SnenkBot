<script lang="ts">
	import DesktopShell from '../src/lib/DesktopShell.svelte';
	import VtubeConnectionView from '../src/lib/VtubeConnectionView.svelte';
	import type {
		ConfigurationSnapshot,
		CredentialPresence,
		ConnectionStatus,
		WorkflowStatus
	} from '../src/lib/contracts/index';

	const parameters = new URLSearchParams(location.search);
	const initialPresence = (parameters.get('presence') ?? 'empty') as CredentialPresence;
	let snapshot = $state<ConfigurationSnapshot>({
		session_id: 'vtube-render-session',
		revision: 2,
		module: 'vtube_studio',
		schema: {
			id: 'snenkbot.vtube.connection',
			version: 1,
			title: 'VTube Studio',
			outputs: [],
			fields: [
				{
					id: 'host',
					label: 'Host',
					description: '',
					introduced_in: 1,
					kind: 'text',
					required: true,
					choice_source: null
				},
				{
					id: 'port',
					label: 'Port',
					description: '',
					introduced_in: 1,
					kind: 'integer',
					required: true,
					choice_source: null
				},
				{
					id: 'enabled',
					label: 'Enabled',
					description: '',
					introduced_in: 1,
					kind: 'toggle',
					required: false,
					choice_source: null
				}
			]
		},
		defaults: { host: 'localhost', port: 8001, enabled: true },
		values: { host: 'localhost', port: 8001, enabled: true }
	});

	const connections: ConnectionStatus[] = [
		{
			integration: 'vtube_studio',
			connection: 'main',
			title: 'VTube Studio',
			state: 'connected',
			detail: 'Actions and events connected'
		}
	];
	const workflows: WorkflowStatus[] = [
		{
			id: 'wave',
			title: 'A little welcome',
			enabled: false,
			trigger_summary: '!hello',
			step_count: 1,
			category: 'Chat',
			capabilities: ['vtube.hotkey.trigger'],
			capability_titles: { 'vtube.hotkey.trigger': 'Trigger hotkey' },
			integration_usage: { vtube_studio: ['Trigger hotkey'] },
			revision: 1,
			has_steps: true,
			error: null
		}
	];
	let delay = $state(parameters.get('state') === 'pending');
	let presence = $state<CredentialPresence>(initialPresence);
	const wait = () =>
		delay ? new Promise<void>((resolve) => setTimeout(resolve, 1500)) : Promise.resolve();
</script>

<DesktopShell
	page="devices"
	title="Devices & Tools"
	{connections}
	onNavigate={() => {}}
	onConfigure={() => {}}
	onDismissError={() => {}}
>
	<div class="configuration-content">
		<VtubeConnectionView
			{snapshot}
			{connections}
			{workflows}
			onOpen={() => {}}
			onSave={async ({ values }) => {
				snapshot = { ...snapshot, revision: snapshot.revision + 1, values };
				return { snapshot, notices: [] };
			}}
			onPresence={async () => {
				await wait();
				return parameters.get('state') === 'error' ? 'unavailable' : presence;
			}}
			onAuthorize={async () => {
				await wait();
				presence = 'stored';
			}}
			onForget={async () => {
				await wait();
				presence = 'empty';
			}}
		/>
	</div>
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
	.configuration-content {
		padding: 24px;
		max-width: 968px;
	}
</style>
