<script lang="ts">
	import DesktopShell from '../src/lib/DesktopShell.svelte';
	import AccountConnectionsView from '../src/lib/AccountConnectionsView.svelte';
	import type {
		AccountAuthentication,
		AccountConnections
	} from '../src/lib/AccountConnectionsView.svelte';
	import { twitchAccounts } from './account-fixture';
	import type { ConnectionStatus } from '../src/lib/contracts/index';
	const parameters = new URLSearchParams(location.search);
	const phase = parameters.get('phase');
	let accounts = $state<AccountConnections>(
		twitchAccounts(
			{ user_id: '123', login: 'faey' },
			parameters.has('bot') ? { user_id: '456', login: 'snenkbot' } : null
		)
	);
	let authentication = $state<AccountAuthentication | null>(
		phase
			? {
					attempt_id: 'render-attempt',
					role: 'bot',
					phase:
						phase === 'review'
							? { state: 'review', data: { login: 'snenkbot', user_id: '456' } }
							: phase === 'saving'
								? { state: 'saving' }
								: phase === 'failed'
									? {
											state: 'failed',
											data: { message: 'Twitch sign-in failed. Please try again.' }
										}
									: {
											state: 'code',
											data: { url: 'https://www.twitch.tv/activate', code: 'ABCDEFGH' }
										}
				}
			: null
	);
	const connections: ConnectionStatus[] = [
		{
			integration: 'twitch',
			connection: 'broadcaster',
			title: 'Twitch Broadcaster Account',
			state: 'connected',
			show_when_healthy: true,
			state_label: 'Signed in',
			detail: 'Signed in as faey'
		}
	];
	const noEffect = () => {};
</script>

<DesktopShell
	page="streaming"
	title="Twitch"
	{connections}
	onNavigate={noEffect}
	onConfigure={noEffect}
	onDismissError={noEffect}
>
	<div class="content">
		<AccountConnectionsView
			{accounts}
			showTitle={false}
			{authentication}
			{connections}
			onStart={async (role) => {
				authentication = {
					attempt_id: 'render-new',
					role,
					phase: {
						state: 'code',
						data: { url: 'https://www.twitch.tv/activate', code: 'ABCDEFGH' }
					}
				};
				return authentication;
			}}
			onOpen={async () => {}}
			onCopy={async () => {}}
			onCancel={async () => {
				if (authentication) authentication = { ...authentication, phase: { state: 'cancelled' } };
			}}
			onApprove={async () => {
				if (authentication) {
					authentication = {
						...authentication,
						phase: { state: 'connected', data: { login: 'snenkbot' } }
					};
					accounts = {
						...accounts,
						roles: accounts.roles.map((slot) =>
							slot.descriptor.id === 'bot'
								? { ...slot, account: { login: 'snenkbot', user_id: '456' } }
								: slot
						)
					};
				}
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
	.content {
		padding: 24px;
		max-width: 968px;
	}
</style>
