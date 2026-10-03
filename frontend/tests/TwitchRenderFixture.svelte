<script lang="ts">
	import DesktopShell from '../src/lib/DesktopShell.svelte';
	import TwitchAccountsView from '../src/lib/TwitchAccountsView.svelte';
	import type {
		TwitchAuthentication,
		TwitchAccountsSnapshot,
		ConnectionStatus
	} from '../src/lib/contracts/index';
	const parameters = new URLSearchParams(location.search);
	const phase = parameters.get('phase');
	let accounts = $state<TwitchAccountsSnapshot>({
		broadcaster: { user_id: '123', login: 'faey' },
		bot: parameters.has('bot') ? { user_id: '456', login: 'snenkbot' } : null
	});
	let authentication = $state<TwitchAuthentication | null>(
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
			detail: 'Signed in as faey'
		}
	];
	const noEffect = () => {};
</script>

<DesktopShell
	page="streaming"
	title="Streaming Services"
	{connections}
	onNavigate={noEffect}
	onConfigure={noEffect}
	onDismissError={noEffect}
>
	<div class="content">
		<TwitchAccountsView
			{accounts}
			{authentication}
			{connections}
			onStart={async ({ role }) => {
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
					accounts = { ...accounts, bot: { login: 'snenkbot', user_id: '456' } };
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
