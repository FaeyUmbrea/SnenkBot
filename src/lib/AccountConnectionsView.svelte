<script lang="ts">
	import { onDestroy, untrack } from 'svelte';
	import type {
		AccountRoleSnapshot,
		ConnectionStatus,
		ReconfigurationRequest,
		TwitchAccountsSnapshot,
		TwitchAuthentication
	} from './contracts/index';
	import { DesktopRequestError } from './desktop';
	import './theme.css';

	/** Public metadata and identity for one account slot supplied by its integration. */
	export type AccountRole = Omit<AccountRoleSnapshot, 'descriptor'> & {
		descriptor: Omit<AccountRoleSnapshot['descriptor'], 'id'> & { id: string };
	};
	/** The integration-owned account list presented by the shared account control. */
	export type AccountConnections = Omit<TwitchAccountsSnapshot, 'roles'> & {
		roles: AccountRole[];
	};
	/** A native device-code attempt adapted to an account role identifier. */
	export type AccountAuthentication = Omit<TwitchAuthentication, 'role'> & { role: string };

	let {
		accounts,
		selectedRole,
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
		accounts: AccountConnections;
		selectedRole?: string;
		authentication: AccountAuthentication | null;
		connections?: readonly ConnectionStatus[];
		requests?: readonly ReconfigurationRequest[];
		onStart: (roleId: string) => Promise<unknown>;
		onApprove: (attemptId: string, userId: string) => Promise<unknown>;
		onCancel: (attemptId: string) => Promise<unknown>;
		onOpen: (attemptId: string) => Promise<unknown>;
		onCopy: (attemptId: string, target: 'link' | 'code') => Promise<unknown>;
		onBack?: () => void;
		backLabel?: string;
	} = $props();

	type Operation = 'start' | 'approve' | 'cancel' | 'open' | 'link' | 'code';
	const roles = $derived(
		selectedRole
			? accounts.roles.filter(({ descriptor }) => descriptor.id === selectedRole)
			: accounts.roles
	);
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

	/** Identifies the current native attempt so late responses cannot change another one. */
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

	/** Uses public account, reconfiguration and connection state in that order. */
	function accountStatus(role: AccountRole) {
		if (!role.account) return role.descriptor.empty_status;
		const request = requests.find(
			(item) =>
				item.integration === accounts.integration_id && item.connection === role.descriptor.id
		);
		if (request) return request.reason;
		const connection = connections.find(
			(item) =>
				item.integration === accounts.integration_id && item.connection === role.descriptor.id
		);
		return connection?.detail || 'Signed in';
	}

	/** Runs one device-code action and discards feedback from an earlier attempt. */
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
					: 'Could not complete this account request. Try again.';
		}
	}

	/** Starts the selected declared role without replacing progress from the native feed. */
	function start(roleId: string) {
		if (cannotStart) return;
		void perform('start', () => onStart(roleId));
	}

	/** Confirms the identity reviewed for the current device-code attempt. */
	function approve() {
		const attempt = authentication;
		if (!attempt || attempt.phase.state !== 'review' || isSaving || pending.cancel) return;
		const userId = attempt.phase.data.user_id;
		void perform('approve', () => onApprove(attempt.attempt_id, userId));
	}

	/** Cancels only an in-progress attempt that has not begun saving. */
	function cancel() {
		if (
			!authentication ||
			!['starting', 'code', 'review'].includes(authentication.phase.state) ||
			isSaving ||
			pending.cancel
		)
			return;
		void perform('cancel', () => onCancel(authentication.attempt_id));
	}

	/** Opens the activation URL for the current attempt through the owning integration. */
	function open() {
		if (!authentication || authentication.phase.state !== 'code' || pending.cancel) return;
		void perform('open', () => onOpen(authentication.attempt_id));
	}

	/** Copies an activation value without exposing it to another attempt. */
	function copy(target: 'link' | 'code') {
		if (!authentication || authentication.phase.state !== 'code' || pending.cancel) return;
		void perform(
			target,
			() => onCopy(authentication.attempt_id, target),
			target === 'link' ? 'Link copied.' : 'Code copied.'
		);
	}
</script>

<section class="workflow-editor account-connections-view" aria-label={`${accounts.title} accounts`}>
	<h1>{accounts.title}</h1>
	<div class="account-list">
		{#each roles as role (role.descriptor.id)}
			{@const descriptor = role.descriptor}
			<section class="account-panel" aria-label={descriptor.label}>
				<h2>
					<span class="account-icon" aria-hidden="true"
					></span>{descriptor.label}{descriptor.optional ? ' · optional' : ''}
				</h2>
				<div class="account-row">
					<p class="account-name">{role.account?.login ?? descriptor.empty_name}</p>
					<button
						class="secondary connect"
						disabled={cannotStart}
						onclick={() => start(descriptor.id)}
						aria-label={`${role.account ? 'Reconnect' : 'Connect'} ${descriptor.label.toLowerCase()}`}
						>{pending.start ? 'Starting…' : role.account ? 'Reconnect' : 'Connect'}</button
					>
				</div>
				<p class="account-status">{accountStatus(role)}</p>
				<p class="account-description">{descriptor.description}</p>
				{#if descriptor.help}<p class="account-guidance">{descriptor.help}</p>{/if}
				{#if authentication?.role === descriptor.id && authentication.phase.state !== 'connected' && authentication.phase.state !== 'cancelled'}
					<div class="authentication-panel" aria-label={`${descriptor.label} sign-in`}>
						{#if authentication.phase.state === 'starting'}
							<p role="status">Preparing sign-in…</p>
						{:else if authentication.phase.state === 'code'}
							<h3>Sign in to your {descriptor.label.toLowerCase()}</h3>
							<p>Open the activation link and enter this code. {accounts.account_selection_hint}</p>
							<label for="account-activation-link">Activation link</label>
							<div class="field-row">
								<input
									id="account-activation-link"
									readonly
									value={authentication.phase.data.url}
								/><button
									class="secondary"
									disabled={pending.link || pending.cancel}
									onclick={() => copy('link')}>{pending.link ? 'Copying…' : 'Copy link'}</button
								>
							</div>
							<label for="account-activation-code">Activation code</label>
							<div class="field-row">
								<input
									id="account-activation-code"
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
								><span role="status">Waiting for sign-in…</span>
							</div>
						{:else if authentication.phase.state === 'review'}
							<h3>Confirm your {descriptor.label.toLowerCase()}</h3>
							<p>
								Use <strong>{authentication.phase.data.login}</strong> as your
								<strong>{descriptor.label.toLowerCase()}</strong>?
							</p>
							<p>Check the account before confirming. {accounts.account_selection_hint}</p>
							<div class="authentication-actions">
								<button disabled={isSaving || pending.cancel} onclick={approve}
									>{pending.approve ? 'Saving…' : 'Confirm account'}</button
								><button
									class="secondary"
									disabled={cannotStart}
									onclick={() => start(descriptor.id)}>Try another account</button
								>
							</div>
						{:else if authentication.phase.state === 'saving'}
							<p role="status">Saving your {descriptor.label.toLowerCase()}…</p>
						{:else if authentication.phase.state === 'failed'}
							<p role="alert">{authentication.phase.data.message}</p>
							<button disabled={cannotStart} onclick={() => start(descriptor.id)}>Try again</button>
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
	{#if onBack}<button class="secondary back" disabled={isSaving} onclick={onBack}
			>{backLabel}</button
		>{/if}
</section>

<style>
	.account-connections-view {
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
		mask: url('../../assets/icons/message-circle.svg') center / contain no-repeat;
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
	.account-guidance {
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
