<script lang="ts">
	import { onDestroy, onMount, untrack } from 'svelte';
	import ConfigurationView from './ConfigurationView.svelte';
	import IntegrationOverview from './IntegrationOverview.svelte';
	import type {
		ConfigurationSaveResult,
		ConfigurationSnapshot,
		ConnectionStatus,
		CredentialPresence,
		SaveConfiguration,
		SaveObsPassword,
		WorkflowStatus
	} from './contracts/index';

	let {
		snapshot,
		connections,
		workflows,
		onOpen,
		onSave,
		onPresence,
		onSavePassword,
		onBack,
		backLabel = 'Back'
	}: {
		snapshot: ConfigurationSnapshot;
		connections: readonly ConnectionStatus[];
		workflows: readonly WorkflowStatus[];
		onOpen: (workflowId: string) => void;
		onSave: (request: SaveConfiguration) => Promise<ConfigurationSaveResult>;
		/** Reads only the startup credential cache, never the operating-system credential store. */
		onPresence: () => Promise<CredentialPresence>;
		onSavePassword: (request: SaveObsPassword) => Promise<CredentialPresence>;
		onBack?: () => void;
		backLabel?: string;
	} = $props();

	let presence = $state<CredentialPresence>('unavailable');
	let checking = $state(false);
	let savingPassword = $state(false);
	let mounted = true;
	let generation = 0;
	let attempt = 0;
	let session = untrack(() => snapshot.session_id);

	function current(requestGeneration: number, requestSession: string) {
		return mounted && generation === requestGeneration && snapshot.session_id === requestSession;
	}

	async function refreshPresence() {
		const requestGeneration = generation;
		const requestSession = snapshot.session_id;
		const requestAttempt = ++attempt;
		checking = true;
		try {
			const value = await onPresence();
			if (current(requestGeneration, requestSession) && requestAttempt === attempt)
				presence = ['empty', 'stored', 'unavailable'].includes(value) ? value : 'unavailable';
		} catch {
			if (current(requestGeneration, requestSession) && requestAttempt === attempt)
				presence = 'unavailable';
		} finally {
			if (current(requestGeneration, requestSession) && requestAttempt === attempt)
				checking = false;
		}
	}

	async function savePassword(request: SaveObsPassword) {
		const requestGeneration = generation;
		const requestSession = snapshot.session_id;
		attempt += 1;
		checking = false;
		savingPassword = true;
		try {
			const value = await onSavePassword(request);
			if (current(requestGeneration, requestSession))
				presence = ['empty', 'stored', 'unavailable'].includes(value) ? value : 'unavailable';
			return value;
		} finally {
			if (current(requestGeneration, requestSession)) savingPassword = false;
		}
	}

	onMount(() => {
		void refreshPresence();
	});
	$effect(() => {
		if (snapshot.session_id === session) return;
		session = snapshot.session_id;
		generation += 1;
		presence = 'unavailable';
		savingPassword = false;
		void refreshPresence();
	});
	onDestroy(() => {
		mounted = false;
		generation += 1;
		attempt += 1;
	});
</script>

<IntegrationOverview integration="obs" {connections} {workflows} {onOpen}>
	<div>
		<ConfigurationView
			{snapshot}
			credentialPresence={presence}
			{onSave}
			onSavePassword={savePassword}
			{onBack}
			{backLabel}
		/>
		{#if checking}
			<p role="status">Checking stored password status…</p>
		{:else if presence === 'unavailable'}
			<div class="password-status">
				<span>Stored password status could not be checked.</span>
				<button disabled={savingPassword} onclick={() => refreshPresence()}
					>Retry password status</button
				>
			</div>
		{/if}
	</div>
</IntegrationOverview>

<style>
	p,
	.password-status {
		color: var(--muted);
		font-size: 12px;
		margin: 16px 0 0;
	}
	.password-status {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 12px;
	}
	button {
		background: var(--surface);
		border: 1px solid var(--border);
		border-radius: 6px;
		color: var(--text);
		padding: 8px 12px;
	}
</style>
