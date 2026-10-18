import type { TwitchAccountSummary, TwitchAccountsSnapshot } from '../src/lib/contracts/index';

/** Builds a public account snapshot without native credentials for UI/controller tests. */
export function twitchAccounts(
	broadcaster: TwitchAccountSummary | null = null,
	bot: TwitchAccountSummary | null = null
): TwitchAccountsSnapshot {
	return {
		integration_id: 'twitch',
		title: 'Twitch',
		account_selection_hint:
			'Use a private or incognito window to choose a different Twitch account.',
		roles: [
			{
				descriptor: {
					id: 'broadcaster',
					label: 'Broadcaster account',
					description: 'Account for channel events and broadcaster actions.',
					optional: false,
					empty_name: 'No account connected',
					empty_status: 'Not signed in',
					help: null
				},
				account: broadcaster
			},
			{
				descriptor: {
					id: 'bot',
					label: 'Bot account',
					description: 'Account used to send chat messages.',
					optional: true,
					empty_name: 'No separate bot account',
					empty_status: 'Uses your broadcaster account',
					help: 'A separate bot account is optional.'
				},
				account: bot
			}
		]
	};
}
