<script lang="ts">
	import { onDestroy, onMount, untrack } from 'svelte';
	import { DesktopRequestError } from './desktop';
	import type { AuthorizationDescriptor, CredentialPresence } from './contracts/index';

	let {
		descriptor,
		ready,
		contextKey,
		onPresence,
		onAuthorize,
		onForget
	}: {
		descriptor: AuthorizationDescriptor;
		ready: boolean;
		contextKey: string;
		onPresence: () => Promise<CredentialPresence>;
		onAuthorize: () => Promise<void>;
		onForget: () => Promise<void>;
	} = $props();

	let presence = $state<CredentialPresence>('unavailable');
	let pending = $state(false);
	let presenceLoading = $state(false);
	let errorMessage = $state('');
	let feedback = $state('');
	let mounted = true;
	let generation = 0;
	let presenceAttempt = 0;
	const identity = $derived(`${descriptor.id}\0${contextKey}`);
	let observedIdentity = untrack(() => identity);

	onMount(() => {
		void refreshPresence(generation, identity);
	});

	$effect(() => {
		const currentIdentity = identity;
		if (currentIdentity === observedIdentity) return;
		observedIdentity = currentIdentity;
		generation += 1;
		presenceAttempt += 1;
		pending = false;
		presence = 'unavailable';
		presenceLoading = false;
		errorMessage = '';
		feedback = '';
		void refreshPresence(generation, currentIdentity);
	});

	onDestroy(() => {
		mounted = false;
		generation += 1;
		presenceAttempt += 1;
	});

	/** Accepts a response only while this authorization context remains mounted and current. */
	function current(requestGeneration: number, requestIdentity: string) {
		return mounted && generation === requestGeneration && identity === requestIdentity;
	}

	/** Shows safe native errors and hides unknown transport details. */
	function safeError(error: unknown, fallback: string) {
		return error instanceof DesktopRequestError ? error.message : fallback;
	}

	/** Reads authorization state and ignores results from earlier attempts or sessions. */
	async function refreshPresence(requestGeneration: number, requestIdentity: string) {
		const attempt = ++presenceAttempt;
		presenceLoading = true;
		try {
			const next = await onPresence();
			if (
				!current(requestGeneration, requestIdentity) ||
				attempt !== presenceAttempt ||
				!['stored', 'empty', 'unavailable'].includes(next)
			)
				return false;
			presence = next;
			return true;
		} catch {
			if (current(requestGeneration, requestIdentity) && attempt === presenceAttempt)
				presence = 'unavailable';
			return false;
		} finally {
			if (current(requestGeneration, requestIdentity) && attempt === presenceAttempt)
				presenceLoading = false;
		}
	}

	/** Retries an unavailable status without changing saved authorization. */
	async function retryPresence() {
		if (pending || presenceLoading) return;
		const requestGeneration = generation;
		const requestIdentity = identity;
		errorMessage = '';
		feedback = '';
		if (
			!(await refreshPresence(requestGeneration, requestIdentity)) &&
			current(requestGeneration, requestIdentity)
		) {
			errorMessage = 'Authorization status is unavailable. Try checking again.';
		}
	}

	/** Runs one pairing change and confirms the resulting saved state before reporting success. */
	async function operate(operation: 'authorize' | 'forget') {
		if (pending || presenceLoading) return;
		if (operation === 'authorize' && !ready) {
			feedback = '';
			errorMessage =
				descriptor.prerequisite_message ?? 'Save the required settings before authorizing.';
			return;
		}
		const requestGeneration = generation;
		const requestIdentity = identity;
		pending = true;
		errorMessage = '';
		feedback = '';
		try {
			if (operation === 'authorize') await onAuthorize();
			else await onForget();
			if (!current(requestGeneration, requestIdentity)) return;
			const verified = await refreshPresence(requestGeneration, requestIdentity);
			if (!current(requestGeneration, requestIdentity)) return;
			if (!verified || (operation === 'authorize' ? presence !== 'stored' : presence !== 'empty')) {
				errorMessage =
					operation === 'authorize'
						? 'Authorization completed, but the stored approval could not be verified. Check its status and try again.'
						: 'The forget request completed, but removal could not be verified. Check its status and try again.';
				return;
			}
			feedback =
				operation === 'authorize'
					? `${descriptor.label} is stored.`
					: `${descriptor.label} was removed.`;
		} catch (error) {
			if (current(requestGeneration, requestIdentity)) {
				errorMessage = safeError(
					error,
					operation === 'authorize'
						? 'Could not authorize. Try again.'
						: 'Could not forget authorization. Try again.'
				);
			}
		} finally {
			if (current(requestGeneration, requestIdentity)) pending = false;
		}
	}
</script>

<div class="authorization-field" aria-busy={pending}>
	{#if errorMessage}
		<div class="authorization-feedback error" role="alert">
			<span>{errorMessage}</span>
			<button
				class="dismiss"
				type="button"
				aria-label="Dismiss error"
				onclick={() => (errorMessage = '')}>×</button
			>
		</div>
	{/if}
	{#if feedback}<p class="authorization-feedback" role="status">{feedback}</p>{/if}
	<div class="authorization-row">
		<div class="authorization-copy">
			<strong>{descriptor.label}</strong>
			{#if presenceLoading}
				<span>Checking authorization…</span>
			{:else if presence === 'stored'}
				<span>Previously paired</span>
			{:else if presence === 'empty'}
				<span>Not authorized</span>
			{:else}
				<span>Authorization status unavailable</span>
			{/if}
			<p>{descriptor.description}</p>
			{#if !ready}<p>
					{descriptor.prerequisite_message ?? 'Save the required settings before authorizing.'}
				</p>{/if}
		</div>
		<div class="authorization-actions">
			{#if presence === 'unavailable'}
				<button
					class="secondary"
					type="button"
					disabled={pending || presenceLoading}
					onclick={retryPresence}>Check status</button
				>
			{/if}
			{#if presence === 'stored'}
				<button class="secondary" type="button" disabled={pending} onclick={() => operate('forget')}
					>{pending ? 'Working…' : 'Forget authorization'}</button
				>
			{/if}
			<button
				type="button"
				disabled={pending || presenceLoading || !ready}
				onclick={() => operate('authorize')}>{pending ? 'Working…' : 'Authorize'}</button
			>
		</div>
	</div>
</div>

<style>
	.authorization-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 20px;
		min-height: 72px;
		margin-top: 24px;
		padding: 16px;
		border: 1px solid var(--border);
		border-radius: 8px;
		background: var(--side);
	}
	.authorization-copy {
		display: grid;
		grid-template-columns: 1fr;
		align-items: center;
		gap: 4px 16px;
		flex: 1;
	}
	.authorization-copy strong {
		font-weight: 600;
	}
	.authorization-copy span {
		color: var(--muted);
	}
	.authorization-copy p {
		grid-column: 1 / -1;
		margin: 0;
		color: var(--muted);
		font-size: 12px;
	}
	.authorization-actions {
		display: flex;
		gap: 8px;
	}
	button {
		min-height: 32px;
		padding: 0 12px;
		border: 1px solid var(--blood);
		border-radius: 4px;
		background: var(--blood);
	}
	button.secondary {
		border-color: var(--border);
		background: var(--surface);
	}
	.authorization-feedback {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
		margin: 0 0 12px;
		padding: 10px 12px;
		background: var(--side);
	}
	.authorization-feedback.error {
		color: #f0b5ae;
	}
	button.dismiss {
		min-width: 28px;
		padding: 0;
		border-color: transparent;
		background: transparent;
		font-size: 20px;
	}
	@media (max-width: 640px) {
		.authorization-row {
			align-items: flex-start;
			flex-direction: column;
		}
		.authorization-copy {
			width: 100%;
			grid-template-columns: 1fr;
		}
		.authorization-copy p {
			grid-column: auto;
		}
	}
</style>
