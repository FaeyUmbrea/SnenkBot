<script lang="ts">
	import { onDestroy, onMount, untrack } from 'svelte';
	import ConfigurationView from './ConfigurationView.svelte';
	import IntegrationOverview from './IntegrationOverview.svelte';
	import { DesktopRequestError } from './desktop';
	import type {
		ConnectionStatus,
		WorkflowStatus,
		ConfigurationSaveResult,
		ConfigurationSnapshot,
		CredentialPresence,
		SaveConfiguration
	} from './contracts/index';
	import './theme.css';

	let {
		snapshot,
		connections,
		workflows,
		onOpen,
		onSave,
		onPresence,
		onAuthorize,
		onForget,
		onBack,
		backLabel = 'Back'
	}: {
		snapshot: ConfigurationSnapshot;
		connections: readonly ConnectionStatus[];
		workflows: readonly WorkflowStatus[];
		onOpen: (workflowId: string) => void;
		onSave: (request: SaveConfiguration) => Promise<ConfigurationSaveResult>;
		onPresence: () => Promise<CredentialPresence>;
		onAuthorize: () => Promise<void>;
		onForget: () => Promise<void>;
		onBack?: () => void;
		backLabel?: string;
	} = $props();

	let presence = $state<CredentialPresence>('unavailable');
	let pending = $state(false);
	let presenceLoading = $state(false);
	let errorMessage = $state('');
	let feedback = $state('');
	let mounted = true;
	let generation = 0;
	let observedSession = untrack(() => snapshot.session_id);
	let presenceAttempt = 0;
	const enabled = $derived(
		Boolean(
			Object.hasOwn(snapshot.values, 'enabled')
				? snapshot.values.enabled
				: snapshot.defaults.enabled
		)
	);

	onMount(() => {
		void refreshPresence(generation, snapshot.session_id);
	});

	$effect(() => {
		const session = snapshot.session_id;
		if (session === observedSession) return;
		observedSession = session;
		generation += 1;
		presenceAttempt += 1;
		pending = false;
		presence = 'unavailable';
		errorMessage = '';
		feedback = '';
		void refreshPresence(generation, session);
	});

	onDestroy(() => {
		mounted = false;
		generation += 1;
		presenceAttempt += 1;
	});

	function current(requestGeneration: number, session: string) {
		return mounted && generation === requestGeneration && snapshot.session_id === session;
	}

	function safeError(error: unknown, fallback: string) {
		return error instanceof DesktopRequestError ? error.message : fallback;
	}

	async function refreshPresence(requestGeneration: number, session: string) {
		const attempt = ++presenceAttempt;
		presenceLoading = true;
		try {
			const next = await onPresence();
			if (
				!current(requestGeneration, session) ||
				attempt !== presenceAttempt ||
				!['stored', 'empty', 'unavailable'].includes(next)
			)
				return false;
			presence = next;
			return true;
		} catch {
			if (current(requestGeneration, session) && attempt === presenceAttempt)
				presence = 'unavailable';
			return false;
		} finally {
			if (current(requestGeneration, session) && attempt === presenceAttempt)
				presenceLoading = false;
		}
	}

	async function retryPresence() {
		if (pending || presenceLoading) return;
		const requestGeneration = generation;
		const session = snapshot.session_id;
		errorMessage = '';
		feedback = '';
		if (
			!(await refreshPresence(requestGeneration, session)) &&
			current(requestGeneration, session)
		) {
			errorMessage = 'Authorization status is unavailable. Try checking again.';
		}
	}

	async function operate(operation: 'authorize' | 'forget') {
		if (pending || presenceLoading) return;
		if (operation === 'authorize' && !enabled) {
			feedback = '';
			errorMessage = 'Enable and save VTube Studio settings before authorizing.';
			return;
		}
		const requestGeneration = generation;
		const session = snapshot.session_id;
		pending = true;
		errorMessage = '';
		feedback = '';
		try {
			if (operation === 'authorize') await onAuthorize();
			else await onForget();
			if (!current(requestGeneration, session)) return;
			const verified = await refreshPresence(requestGeneration, session);
			if (!current(requestGeneration, session)) return;
			if (!verified || (operation === 'authorize' ? presence !== 'stored' : presence !== 'empty')) {
				errorMessage =
					operation === 'authorize'
						? 'Authorization completed, but the stored approval could not be verified. Check its status and try again.'
						: 'The forget request completed, but removal could not be verified. Check its status and try again.';
				return;
			}
			feedback =
				operation === 'authorize'
					? 'VTube Studio authorization is stored.'
					: 'VTube Studio authorization was removed.';
		} catch (error) {
			if (current(requestGeneration, session)) {
				errorMessage = safeError(
					error,
					operation === 'authorize'
						? 'Could not authorize VTube Studio. Try again.'
						: 'Could not forget VTube Studio authorization. Try again.'
				);
			}
		} finally {
			if (current(requestGeneration, session)) pending = false;
		}
	}
</script>

<section
	class="workflow-editor vtube-connection-view"
	aria-label="VTube Studio connection"
	aria-busy={pending}
>
	<IntegrationOverview integration="vtube_studio" {connections} {workflows} {onOpen}>
		{#if errorMessage}
			<div class="connection-feedback" role="alert">
				<span>{errorMessage}</span>
				<button
					class="dismiss"
					type="button"
					aria-label="Dismiss error"
					onclick={() => (errorMessage = '')}>×</button
				>
			</div>
		{/if}
		{#if feedback}<p class="connection-status" role="status">{feedback}</p>{/if}
		<ConfigurationView {snapshot} {onSave} {onBack} {backLabel} />
		<div class="authorization-row">
			<div class="authorization-copy">
				<strong>Authorization</strong>
				{#if presenceLoading}
					<span>Checking authorization…</span>
				{:else if presence === 'stored'}
					<span>Previously paired</span>
				{:else if presence === 'empty'}
					<span>Not authorized</span>
				{:else}
					<span>Authorization status unavailable</span>
				{/if}
				<p>VTube Studio will ask you to approve the connection when you authorize.</p>
				{#if !enabled}<p>Enable and save VTube Studio settings before authorizing.</p>{/if}
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
					<button
						class="secondary"
						type="button"
						disabled={pending}
						onclick={() => operate('forget')}
						>{pending ? 'Working…' : 'Forget authorization'}</button
					>
				{/if}
				<button
					type="button"
					disabled={pending || presenceLoading || !enabled}
					onclick={() => operate('authorize')}>{pending ? 'Working…' : 'Authorize'}</button
				>
			</div>
		</div>
	</IntegrationOverview>
</section>

<style>
	.vtube-connection-view {
		width: 100%;
		max-width: 968px;
	}
	.authorization-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 20px;
		min-height: 72px;
		margin-top: 24px;
		padding: 12px 16px;
		background: var(--side);
	}
	.authorization-copy {
		display: grid;
		grid-template-columns: minmax(140px, 1fr) minmax(180px, 1fr);
		align-items: center;
		gap: 4px 16px;
		flex: 1;
	}
	.authorization-copy strong {
		font-weight: 400;
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
	.connection-feedback,
	.connection-status {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
		margin: 0 0 12px;
		padding: 10px 12px;
		background: var(--side);
	}
	.connection-feedback {
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
