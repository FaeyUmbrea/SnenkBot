<script lang="ts">
	import { untrack } from 'svelte';
	import { DesktopRequestError } from './desktop';
	import ConfigurationFields from './ConfigurationFields.svelte';
	import CredentialField from './CredentialField.svelte';
	import AuthorizationField from './AuthorizationField.svelte';
	import type { CredentialDescriptor, SaveCredential } from './credential';
	import type {
		ConfigurationSaveResult,
		ConfigurationSnapshot,
		CredentialPresence,
		SaveConfiguration
	} from './contracts/index';
	import './theme.css';

	let {
		snapshot,
		credentialPresence = 'unavailable',
		credential,
		onSave,
		onSaveCredential,
		onRefreshCredential,
		onAuthorizationPresence,
		onAuthorize,
		onForgetAuthorization,
		onBack,
		pickDirectory,
		backLabel = 'Back'
	}: {
		snapshot: ConfigurationSnapshot;
		credentialPresence?: CredentialPresence;
		credential?: CredentialDescriptor;
		onSave: (request: SaveConfiguration) => Promise<ConfigurationSaveResult>;
		onSaveCredential?: SaveCredential;
		onRefreshCredential?: () => Promise<CredentialPresence>;
		onAuthorizationPresence?: () => Promise<CredentialPresence>;
		onAuthorize?: () => Promise<void>;
		onForgetAuthorization?: () => Promise<void>;
		onBack?: () => void;
		pickDirectory?: (field_path: string[], current_path: string | null) => Promise<string | null>;
		backLabel?: string;
	} = $props();

	let draft = $state<Record<string, unknown>>(untrack(() => valuesFromSnapshot(snapshot)));
	let revision = $state(untrack(() => snapshot.revision));
	let sessionId = $state(untrack(() => snapshot.session_id));
	let isDirty = $state(false);
	let hasRevisionConflict = $state(false);
	let isSaving = $state(false);
	let isSavingCredential = $state(false);
	let editVersion = $state(0);
	let message = $state('');
	let errorMessage = $state('');
	let notices = $state<string[]>([]);

	const portableFields = $derived(
		snapshot.schema.fields.filter((field) => field.kind !== 'secret')
	);

	function valuesFromSnapshot(value: ConfigurationSnapshot) {
		return Object.fromEntries(
			value.schema.fields
				.filter((field) => field.kind !== 'secret')
				.map((field) => {
					if (Object.hasOwn(value.values, field.id)) {
						return [field.id, value.values[field.id] ?? (field.kind === 'toggle' ? false : '')];
					}
					if (Object.hasOwn(value.defaults, field.id)) {
						return [field.id, value.defaults[field.id] ?? (field.kind === 'toggle' ? false : '')];
					}
					return [field.id, field.kind === 'toggle' ? false : ''];
				})
		);
	}

	$effect(() => {
		if (snapshot.session_id !== sessionId) {
			sessionId = snapshot.session_id;
			revision = snapshot.revision;
			draft = valuesFromSnapshot(snapshot);
			isDirty = false;
			isSaving = false;
			isSavingCredential = false;
			hasRevisionConflict = false;
			message = '';
			errorMessage = '';
			notices = [];
			return;
		}
		if (snapshot.revision > revision) {
			if (isDirty) {
				if (!isSaving) hasRevisionConflict = true;
			} else {
				revision = snapshot.revision;
				draft = valuesFromSnapshot(snapshot);
			}
		}
	});

	/** Update the shared schema editor draft and retain optimistic-save bookkeeping. */
	function updateFields(values: Record<string, unknown>) {
		draft = values;
		isDirty = true;
		editVersion += 1;
		message = '';
		errorMessage = '';
		notices = [];
	}

	function reloadSnapshot() {
		draft = valuesFromSnapshot(snapshot);
		revision = snapshot.revision;
		hasRevisionConflict = false;
		isDirty = false;
		editVersion += 1;
		message = '';
		errorMessage = '';
		notices = [];
	}

	async function save() {
		if (isSaving || hasRevisionConflict) return;
		const requestSessionId = sessionId;
		const requestRevision = revision;
		const requestValues = { ...draft };
		const requestEditVersion = editVersion;
		isSaving = true;
		message = '';
		errorMessage = '';
		notices = [];
		try {
			const result = await onSave({
				session_id: requestSessionId,
				expected_revision: requestRevision,
				values: requestValues
			});
			if (
				snapshot.session_id !== requestSessionId ||
				result.snapshot.session_id !== requestSessionId
			)
				return;
			revision = result.snapshot.revision;
			hasRevisionConflict = snapshot.revision > result.snapshot.revision;
			if (editVersion === requestEditVersion && !hasRevisionConflict) {
				draft = valuesFromSnapshot(result.snapshot);
			}
			notices = result.notices.map((notice) =>
				notice === 'directory_sync'
					? 'Settings were saved, but the containing directory could not be synced.'
					: 'Settings were saved, but old backups could not be fully cleaned up.'
			);
			if (editVersion === requestEditVersion && !hasRevisionConflict) isDirty = false;
			if (!notices.length) message = 'Settings saved.';
		} catch (error) {
			if (snapshot.session_id !== requestSessionId) return;
			if (snapshot.revision > requestRevision) hasRevisionConflict = true;
			errorMessage =
				error instanceof DesktopRequestError
					? error.message
					: 'Settings could not be saved. Your entries are still available.';
		} finally {
			if (snapshot.session_id === requestSessionId) isSaving = false;
		}
	}
</script>

<section
	class="workflow-editor configuration-view"
	aria-label={`${snapshot.schema.title} settings`}
	aria-busy={isSaving || isSavingCredential}
>
	<header class="configuration-header">
		<div>
			<h1>{snapshot.schema.title}</h1>
		</div>
	</header>
	<form
		onsubmit={(event) => {
			event.preventDefault();
			void save();
		}}
	>
		<div class="configuration-fields">
			<ConfigurationFields
				fields={portableFields}
				values={draft}
				{pickDirectory}
				onChange={updateFields}
			/>
			{#if credential && onSaveCredential}
				<CredentialField
					descriptor={credential}
					contextKey={snapshot.session_id}
					presence={credentialPresence}
					onSave={onSaveCredential}
					onRefresh={onRefreshCredential}
					onBusyChange={(busy) => (isSavingCredential = busy)}
				/>
			{/if}
		</div>
		{#if hasRevisionConflict}
			<p role="alert">
				These settings changed in another view. Reload the current values before saving.
			</p>
			<button type="button" class="secondary" onclick={reloadSnapshot} disabled={isSaving}
				>Reload current values</button
			>
		{/if}
		{#if errorMessage}<div class="save-error">
				<p role="alert">{errorMessage}</p>
				<button
					type="button"
					class="secondary"
					aria-label="Dismiss save error"
					onclick={() => (errorMessage = '')}>Dismiss</button
				>
			</div>{/if}
		{#if message}<p role="status">{message}</p>{/if}
		{#each notices as notice, index (index)}<p role="status">{notice}</p>{/each}
		<footer class="configuration-actions">
			{#if onBack}<button
					type="button"
					class="secondary"
					onclick={onBack}
					disabled={isSaving || isSavingCredential}>{backLabel}</button
				>{/if}
			<button type="submit" disabled={isSaving || hasRevisionConflict}
				>{isSaving ? 'Saving…' : 'Save settings'}</button
			>
		</footer>
	</form>
	{#if snapshot.authorization && onAuthorizationPresence && onAuthorize && onForgetAuthorization}
		<AuthorizationField
			descriptor={snapshot.authorization}
			ready={snapshot.authorization_ready}
			contextKey={snapshot.session_id}
			onPresence={onAuthorizationPresence}
			{onAuthorize}
			onForget={onForgetAuthorization}
		/>
	{/if}
</section>

<style>
	.save-error {
		display: flex;
		align-items: center;
		gap: 12px;
		margin-top: 12px;
	}
	.save-error p {
		flex: 1;
		margin: 0;
		overflow-wrap: anywhere;
	}
	.configuration-view {
		width: 100%;
	}
	.configuration-header,
	.configuration-actions {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 16px;
	}
	.configuration-header {
		margin-bottom: 16px;
	}
	h1 {
		margin: 0;
		font-size: 16px;
		font-weight: 600;
	}
	.configuration-fields {
		display: flex;
		flex-direction: column;
		gap: 0;
		padding: 0 16px;
		background: var(--side);
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
		background: var(--side);
	}
	.configuration-actions {
		justify-content: flex-end;
		margin-top: 16px;
	}
	@media (max-width: 640px) {
		.configuration-header {
			align-items: flex-start;
		}
	}
</style>
