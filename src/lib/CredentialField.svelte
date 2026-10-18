<script lang="ts">
	import { onMount, untrack } from 'svelte';
	import { DesktopRequestError } from './desktop';
	import type { CredentialPresence } from './contracts/index';
	import type { CredentialDescriptor, SaveCredential } from './credential';

	let {
		descriptor,
		contextKey,
		presence = 'unavailable',
		onSave,
		onRefresh,
		onBusyChange
	}: {
		descriptor: CredentialDescriptor;
		contextKey: string;
		presence?: CredentialPresence;
		onSave: SaveCredential;
		onRefresh?: () => Promise<CredentialPresence>;
		onBusyChange?: (busy: boolean) => void;
	} = $props();

	let draft = $state('');
	let currentPresence = $state<CredentialPresence>(untrack(() => presence));
	let busy = $state(false);
	let checking = $state(false);
	let error = $state('');
	let message = $state('');
	let editVersion = 0;
	let requestVersion = 0;
	let currentContext = untrack(() => `${descriptor.id}\0${contextKey}`);
	let mounted = true;
	const identity = $derived(`${descriptor.id}\0${contextKey}`);

	$effect(() => {
		if (identity !== currentContext) {
			currentContext = identity;
			requestVersion += 1;
			draft = '';
			currentPresence = presence;
			busy = false;
			checking = false;
			error = '';
			message = '';
			onBusyChange?.(false);
			if (onRefresh) void refresh();
			return;
		}
		if (presence !== 'unavailable') currentPresence = presence;
	});

	/** Refresh only the storage status and ignore replies from an older context or attempt. */
	async function refresh() {
		if (!onRefresh || busy) return;
		const requestContext = identity;
		const version = ++requestVersion;
		checking = true;
		try {
			const result = await onRefresh();
			if (mounted && identity === requestContext && version === requestVersion)
				currentPresence = result;
		} catch {
			if (mounted && identity === requestContext && version === requestVersion)
				currentPresence = 'unavailable';
		} finally {
			if (mounted && identity === requestContext && version === requestVersion) checking = false;
		}
	}

	/** Store or remove a secret, retaining newer edits and failed drafts. */
	async function save(remove = false) {
		if (busy || (!remove && !draft) || (remove && currentPresence !== 'stored')) return;
		const requestContext = identity;
		const requestEditVersion = editVersion;
		const submitted = draft;
		const version = ++requestVersion;
		busy = true;
		checking = false;
		onBusyChange?.(true);
		error = '';
		message = '';
		try {
			const result = await onSave(remove ? null : submitted);
			if (!mounted || identity !== requestContext || version !== requestVersion) return;
			if (result !== (remove ? 'empty' : 'stored'))
				throw new Error('Credential storage did not confirm the requested change.');
			currentPresence = result;
			if (editVersion === requestEditVersion) draft = '';
			message = remove
				? (descriptor.removedMessage ?? 'Credential removed.')
				: descriptor.savedMessage;
		} catch (cause) {
			if (!mounted || identity !== requestContext || version !== requestVersion) return;
			error = cause instanceof DesktopRequestError ? cause.message : descriptor.saveError;
		} finally {
			if (mounted && identity === requestContext && version === requestVersion) {
				busy = false;
				onBusyChange?.(false);
			}
		}
	}

	/** Update the secret draft from either typing or a committed input change. */
	function updateDraft(event: Event) {
		draft = (event.currentTarget as HTMLInputElement).value;
		editVersion += 1;
		error = '';
		message = '';
	}

	onMount(() => {
		if (onRefresh) void refresh();
		return () => {
			mounted = false;
			requestVersion += 1;
		};
	});
</script>

<div class="field-row credential-field">
	<div class="field-copy">
		<div class="credential-heading">
			<label for={`credential-${descriptor.id}`}>{descriptor.label}</label><span
				class="presence"
				class:stored={currentPresence === 'stored'}
				>{checking
					? 'Checking…'
					: currentPresence === 'stored'
						? 'Saved'
						: currentPresence === 'empty'
							? 'Not set'
							: 'Status unavailable'}</span
			>
		</div>
		<p>
			{currentPresence === 'stored'
				? descriptor.storedDescription
				: currentPresence === 'empty'
					? descriptor.emptyDescription
					: descriptor.unavailableDescription}
		</p>
	</div>
	<div class="credential-controls">
		<input
			id={`credential-${descriptor.id}`}
			type="password"
			autocomplete="new-password"
			placeholder={currentPresence === 'stored' ? '•••••••• (saved)' : 'Enter a value'}
			value={draft}
			oninput={updateDraft}
			onchange={updateDraft}
			onkeydown={(event) => {
				if (event.key === 'Enter') {
					event.preventDefault();
					void save();
				}
			}}
		/>
		<button type="button" disabled={busy || !draft} onclick={() => save()}>
			{busy ? 'Saving…' : descriptor.saveLabel}
		</button>
		{#if descriptor.removeLabel}<button
				type="button"
				disabled={busy || currentPresence !== 'stored'}
				onclick={() => save(true)}>{descriptor.removeLabel}</button
			>{/if}
		{#if onRefresh && currentPresence === 'unavailable'}<button
				type="button"
				disabled={busy || checking}
				onclick={() => refresh()}>Retry credential status</button
			>{/if}
	</div>
</div>
{#if error}<div class="credential-feedback">
		<p role="alert">{error}</p>
		<button type="button" aria-label="Dismiss save error" onclick={() => (error = '')}
			>Dismiss</button
		>
	</div>{/if}
{#if message}<p role="status">{message}</p>{/if}

<style>
	.credential-heading {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 16px;
	}
	.presence {
		color: var(--muted);
		font-size: 12px;
	}
	.presence.stored {
		color: #79bd8e;
	}
	.field-row {
		display: grid;
		grid-template-columns: 1fr;
		gap: 16px;
		align-items: center;
		min-height: 48px;
		padding: 0;
	}
	.field-copy p {
		margin: 4px 0 0;
		color: var(--muted);
		font-size: 12px;
	}
	.credential-controls {
		display: flex;
		gap: 8px;
		align-items: center;
		flex-wrap: wrap;
	}
	input {
		flex: 1;
		min-width: 160px;
		height: 32px;
		padding: 4px 8px;
		background: var(--surface);
		border: 1px solid var(--border);
		border-radius: 4px;
	}
	button {
		min-height: 32px;
		padding: 0 12px;
		border: 1px solid var(--border);
		border-radius: 4px;
		background: var(--side);
	}
	.credential-feedback {
		display: flex;
		align-items: center;
		gap: 12px;
	}
	.credential-feedback p {
		flex: 1;
		overflow-wrap: anywhere;
	}
	@media (max-width: 640px) {
		.field-row {
			grid-template-columns: 1fr;
			gap: 8px;
		}
	}
</style>
