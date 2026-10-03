<script lang="ts">
	import { onDestroy, untrack } from 'svelte';
	import type {
		ApproveTwitchLogin,
		AuthenticationIdentity,
		ConnectionStatus,
		CopyTwitchLogin,
		ReconfigurationRequest,
		StartTwitchLogin,
		TwitchAccountsSnapshot,
		TwitchAuthentication,
		TwitchRole
	} from './contracts/index';
	import { DesktopRequestError } from './desktop';
	import './theme.css';

	let {
		accounts,
		authentication,
		connections = [],
		requests = [],
		onStart,
		onApprove,
		onCancel,
		onOpen,
		onCopy,
		onBack,
		backLabel = 'Back'
	}: {
		accounts: TwitchAccountsSnapshot;
		authentication: TwitchAuthentication | null;
		connections?: readonly ConnectionStatus[];
		requests?: readonly ReconfigurationRequest[];
		onStart: (request: StartTwitchLogin) => Promise<TwitchAuthentication>;
		onApprove: (request: ApproveTwitchLogin) => Promise<void>;
		onCancel: (request: AuthenticationIdentity) => Promise<void>;
		onOpen: (request: AuthenticationIdentity) => Promise<void>;
		onCopy: (request: CopyTwitchLogin) => Promise<void>;
		onBack?: () => void;
		backLabel?: string;
	} = $props();

	type Operation = 'start' | 'approve' | 'cancel' | 'open' | 'link' | 'code';
	const roles: TwitchRole[] = ['broadcaster', 'bot'];
	let pending = $state<Partial<Record<Operation, boolean>>>({});
	let feedback = $state('');
	let errorMessage = $state('');
	let generation = 0;
	let mounted = true;
	let observedKey = untrack(() => authenticationKey());
	const isSaving = $derived(authentication?.phase.state === 'saving' || !!pending.approve);
	const cannotStart = $derived(
		isSaving || authentication?.phase.state === 'starting' || !!pending.start || !!pending.cancel
	);

	function authenticationKey() {
		return authentication ? `${authentication.attempt_id}:${authentication.phase.state}` : '';
	}

	$effect(() => {
		const key = authenticationKey();
		if (key === observedKey) return;
		observedKey = key;
		generation += 1;
		pending = {};
		feedback = '';
		errorMessage = '';
	});

	onDestroy(() => {
		mounted = false;
		generation += 1;
	});

	function roleTitle(role: TwitchRole) {
		return role === 'broadcaster' ? 'Broadcaster account' : 'Bot account';
	}

	function accountStatus(role: TwitchRole) {
		const account = accounts[role];
		if (!account) return role === 'bot' ? 'Uses your broadcaster account' : 'Not signed in';
		const request = requests.find(
			(item) => item.integration === 'twitch' && item.connection === role
		);
		if (request) return request.reason;
		const connection = connections.find(
			(item) => item.integration === 'twitch' && item.connection === role
		);
		return connection?.detail || 'Signed in';
	}

	async function perform(operation: Operation, action: () => Promise<unknown>, success = '') {
		if (pending[operation]) return;
		const key = authenticationKey();
		const requestGeneration = generation;
		pending = { ...pending, [operation]: true };
		feedback = '';
		errorMessage = '';
		const isCurrent = () =>
			mounted && generation === requestGeneration && authenticationKey() === key;
		try {
			await action();
			if (!isCurrent()) return;
			feedback = success;
			// Approval and cancellation stay guarded until the backend changes the phase.
			if (operation !== 'approve' && operation !== 'cancel') {
				pending = { ...pending, [operation]: false };
			}
		} catch (error) {
			if (!isCurrent()) return;
			pending = { ...pending, [operation]: false };
			errorMessage =
				error instanceof DesktopRequestError
					? error.message
					: 'Could not complete this Twitch request. Try again.';
		}
	}

	function start(role: TwitchRole) {
		if (cannotStart) return;
		// Progress comes from the feed; a delayed command response must not replace it.
		void perform('start', () => onStart({ role }));
	}

	function approve() {
		if (!authentication || authentication.phase.state !== 'review' || isSaving || pending.cancel)
			return;
		const request = {
			attempt_id: authentication.attempt_id,
			user_id: authentication.phase.data.user_id
		};
		void perform('approve', () => onApprove(request));
	}

	function cancel() {
		if (
			!authentication ||
			!['starting', 'code', 'review'].includes(authentication.phase.state) ||
			isSaving ||
			pending.cancel
		)
			return;
		const request = { attempt_id: authentication.attempt_id };
		void perform('cancel', () => onCancel(request));
	}

	function open() {
		if (!authentication || authentication.phase.state !== 'code' || pending.cancel) return;
		const request = { attempt_id: authentication.attempt_id };
		void perform('open', () => onOpen(request));
	}

	function copy(target: 'link' | 'code') {
		if (!authentication || authentication.phase.state !== 'code' || pending.cancel) return;
		const request = { attempt_id: authentication.attempt_id, target };
		void perform(
			target,
			() => onCopy(request),
			target === 'link' ? 'Link copied.' : 'Code copied.'
		);
	}
</script>

<section class="workflow-editor twitch-view" aria-label="Twitch accounts">
	<h1>Twitch</h1>
	<div class="account-list">
		{#each roles as role (role)}
			<section class="account-panel" aria-label={roleTitle(role)}>
				<h2>
					<span class="account-icon" aria-hidden="true"></span>{roleTitle(role)}{role === 'bot'
						? ' · optional'
						: ''}
				</h2>
				<div class="account-row">
					<p class="account-name">
						{accounts[role]?.login ??
							(role === 'bot' ? 'No separate bot account' : 'No account connected')}
					</p>
					<button
						class="secondary connect"
						disabled={cannotStart}
						onclick={() => start(role)}
						aria-label={`${accounts[role] ? 'Reconnect' : 'Connect'} ${roleTitle(role).toLowerCase()}`}
						>{pending.start ? 'Starting…' : accounts[role] ? 'Reconnect' : 'Connect'}</button
					>
				</div>
				<p class="account-status">{accountStatus(role)}</p>
				<p class="account-description">
					{role === 'broadcaster'
						? 'Account for channel events and broadcaster actions.'
						: 'Account used to send chat messages.'}
				</p>
				{#if authentication?.role === role && authentication.phase.state !== 'connected' && authentication.phase.state !== 'cancelled'}
					<div class="authentication-panel" aria-label={`${roleTitle(role)} sign-in`}>
						{#if authentication.phase.state === 'starting'}
							<p role="status">Preparing Twitch sign-in…</p>
						{:else if authentication.phase.state === 'code'}
							<h3>Sign in to your {roleTitle(role).toLowerCase()}</h3>
							<p>
								Open the activation link and enter this code. Use a private or incognito window to
								choose a different Twitch account.
							</p>
							<label for="twitch-activation-link">Activation link</label>
							<div class="field-row">
								<input
									id="twitch-activation-link"
									readonly
									value={authentication.phase.data.url}
								/><button
									class="secondary"
									disabled={pending.link || pending.cancel}
									onclick={() => copy('link')}>{pending.link ? 'Copying…' : 'Copy link'}</button
								>
							</div>
							<label for="twitch-activation-code">Activation code</label>
							<div class="field-row">
								<input
									id="twitch-activation-code"
									class="activation-code"
									readonly
									value={authentication.phase.data.code}
								/><button
									class="secondary"
									disabled={pending.code || pending.cancel}
									onclick={() => copy('code')}>{pending.code ? 'Copying…' : 'Copy code'}</button
								>
							</div>
							<div class="authentication-actions">
								<button disabled={pending.open || pending.cancel} onclick={open}
									>{pending.open ? 'Opening…' : 'Open in browser'}</button
								><span role="status">Waiting for Twitch sign-in…</span>
							</div>
						{:else if authentication.phase.state === 'review'}
							<h3>Confirm your {roleTitle(role).toLowerCase()}</h3>
							<p>
								Use <strong>{authentication.phase.data.login}</strong> as your
								<strong>{roleTitle(role).toLowerCase()}</strong>?
							</p>
							<p>
								Check the account before confirming. To choose a different account, try again in a
								private or incognito window.
							</p>
							<div class="authentication-actions">
								<button disabled={isSaving || pending.cancel} onclick={approve}
									>{pending.approve ? 'Saving…' : 'Confirm account'}</button
								><button class="secondary" disabled={cannotStart} onclick={() => start(role)}
									>Try another account</button
								>
							</div>
						{:else if authentication.phase.state === 'saving'}
							<p role="status">Saving your {roleTitle(role).toLowerCase()}…</p>
						{:else if authentication.phase.state === 'failed'}
							<p role="alert">{authentication.phase.data.message}</p>
							<button disabled={cannotStart} onclick={() => start(role)}>Try again</button>
						{/if}
						{#if authentication.phase.state !== 'saving' && authentication.phase.state !== 'failed'}<button
								class="secondary cancel"
								disabled={isSaving || pending.cancel}
								onclick={cancel}>{pending.cancel ? 'Cancelling…' : 'Cancel'}</button
							>{/if}
						{#if errorMessage}<p role="alert" class="feedback">{errorMessage}</p>{/if}
						{#if feedback}<p role="status" class="feedback">{feedback}</p>{/if}
					</div>
				{/if}
			</section>
		{/each}
	</div>
	{#if !authentication || authentication.phase.state === 'connected' || authentication.phase.state === 'cancelled'}
		{#if errorMessage}<p role="alert" class="feedback">{errorMessage}</p>{/if}
		{#if feedback}<p role="status" class="feedback">{feedback}</p>{/if}
	{/if}
	<p class="bot-guidance">
		A separate bot account is optional. Without one, SnenkBot uses your broadcaster account for bot
		features such as sending chat messages. For the best experience, use your broadcaster account or
		make your bot a moderator in your channel—Twitch gives these accounts higher chat message
		limits.
	</p>
	{#if onBack}<button class="secondary back" disabled={isSaving} onclick={onBack}
			>{backLabel}</button
		>{/if}
</section>

<style>
	.twitch-view {
		width: 100%;
		max-width: 920px;
		padding-top: 16px;
	}
	h1 {
		margin: 0 0 28px;
		line-height: 1.3;
		font-size: 16px;
		font-weight: 600;
	}
	.account-list {
		display: flex;
		flex-direction: column;
		gap: 32px;
	}
	.account-panel {
		padding: 20px;
		min-height: 211px;
		border-radius: 8px;
		background: var(--side);
	}
	h2 {
		display: flex;
		align-items: center;
		gap: 16px;
		margin: 0 0 20px;
		font-size: 16px;
		font-weight: 600;
	}
	.account-icon {
		width: 18px;
		height: 18px;
		flex: none;
		background: var(--gold);
		mask: url('../../../assets/icons/message-circle.svg') center / contain no-repeat;
	}
	.account-row {
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: 16px;
	}
	.account-name {
		margin: 0;
		font-size: 20px;
		overflow-wrap: anywhere;
	}
	.account-status,
	.account-description {
		color: var(--muted);
		font-size: 13px;
	}
	.account-status {
		margin: 12px 0 24px;
	}
	.account-description {
		margin: 0;
	}
	button {
		min-height: 32px;
		padding: 0 12px;
		border: 1px solid var(--maroon);
		border-radius: 6px;
		background: var(--maroon);
	}
	button.secondary {
		border-color: transparent;
		background: var(--surface);
	}
	button.connect {
		min-width: 156px;
	}
	.bot-guidance {
		margin: 32px 0 0;
		max-width: 600px;
		color: var(--muted);
		line-height: 1.25;
	}
	.authentication-panel {
		margin-top: 24px;
		padding-top: 20px;
		border-top: 1px solid var(--border);
	}
	h3 {
		margin: 0;
		font-size: 16px;
		font-weight: 600;
	}
	.authentication-panel p {
		max-width: 600px;
		line-height: 1.4;
	}
	label {
		display: block;
		margin-top: 16px;
		margin-bottom: 6px;
	}
	.field-row {
		display: flex;
		gap: 8px;
		max-width: 600px;
	}
	.field-row input {
		width: 100%;
		min-width: 0;
		height: 32px;
		padding: 4px 8px;
		border: 1px solid var(--border);
		border-radius: 4px;
		background: var(--field);
	}
	.field-row button {
		flex: none;
		min-width: 88px;
	}
	.activation-code {
		font-weight: 600;
		letter-spacing: 2px;
	}
	.authentication-actions {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 12px;
		margin-top: 16px;
	}
	.authentication-actions span {
		color: var(--muted);
		font-size: 13px;
	}
	.cancel,
	.back {
		margin-top: 16px;
	}
	.feedback {
		margin: 20px 0 0;
	}
	@media (max-width: 640px) {
		.account-row {
			align-items: flex-start;
			flex-direction: column;
		}
		.account-panel {
			min-height: 211px;
		}
		button.connect {
			min-width: 120px;
		}
	}
</style>
