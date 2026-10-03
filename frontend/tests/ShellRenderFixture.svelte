<script lang="ts">
	import DesktopShell from '../src/lib/DesktopShell.svelte';
	import ObsConnectionView from '../src/lib/ObsConnectionView.svelte';
	import type { ConfigurationSnapshot, ConnectionStatus } from '../src/lib/contracts/index';
	const fixturePresence = new URLSearchParams(location.search).get('presence');
	let snapshot = $state<ConfigurationSnapshot>({
		session_id: 'render-fixture',
		revision: 0,
		module: 'obs',
		schema: {
			id: 'snenkbot.obs.connection',
			version: 1,
			title: 'OBS Studio',
			outputs: [],
			fields: [
				{
					id: 'enabled',
					label: 'Enabled',
					description: '',
					introduced_in: 1,
					kind: 'toggle',
					required: true,
					choice_source: null
				},
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
					id: 'tls',
					label: 'Secure connection',
					description: '',
					introduced_in: 1,
					kind: 'toggle',
					required: true,
					choice_source: null
				}
			]
		},
		defaults: { enabled: false, host: '127.0.0.1', port: 4455, tls: false },
		values: { enabled: true, host: '192.168.178.44', port: 4455, tls: false }
	});
	const connections: ConnectionStatus[] = [
		{
			integration: 'obs',
			connection: 'main',
			title: 'OBS Studio',
			state: 'connected',
			detail: 'Connected to 192.168.178.44:4455'
		},
		{
			integration: 'twitch',
			connection: 'broadcaster',
			title: 'Twitch Broadcaster Account',
			state: 'connected',
			detail: 'Chat and channel events connected'
		},
		{
			integration: 'twitch',
			connection: 'bot',
			title: 'Twitch Bot Account',
			state: 'error',
			detail: 'Reconnect this account to send messages as the bot.'
		}
	];
	const noEffect = () => {};
</script>

<DesktopShell
	page="broadcast"
	title="Broadcast Apps"
	{connections}
	onNavigate={noEffect}
	onConfigure={noEffect}
	onDismissError={noEffect}
>
	<div class="configuration-content">
		<ObsConnectionView
			{connections}
			workflows={[]}
			onOpen={noEffect}
			{snapshot}
			onPresence={async () =>
				fixturePresence === 'unavailable'
					? 'unavailable'
					: fixturePresence === 'empty'
						? 'empty'
						: 'stored'}
			onSave={async (request) => {
				snapshot = { ...snapshot, revision: snapshot.revision + 1, values: request.values };
				return { snapshot, notices: [] };
			}}
			onSavePassword={async () => 'stored'}
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
