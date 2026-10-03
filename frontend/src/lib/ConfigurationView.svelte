<script lang="ts">
	import { untrack } from 'svelte';
	import { DesktopRequestError } from './desktop';
	import type {
		ConfigurationSaveResult,
		ConfigurationSnapshot,
		CredentialPresence,
		SaveConfiguration,
		SaveObsPassword
	} from './contracts/index';
	import './theme.css';

	let {
		snapshot,
		credentialPresence = 'empty',
		onSave,
		onSavePassword,
		onBack,
		backLabel = 'Back'
	}: {
		snapshot: ConfigurationSnapshot;
		credentialPresence?: CredentialPresence;
		onSave: (request: SaveConfiguration) => Promise<ConfigurationSaveResult>;
		onSavePassword?: (request: SaveObsPassword) => Promise<CredentialPresence>;
		onBack?: () => void;
		backLabel?: string;
	} = $props();

	let draft = $state<Record<string, unknown>>(untrack(() => valuesFromSnapshot(snapshot)));
	let passwordDraft = $state('');
	let revision = $state(untrack(() => snapshot.revision));
	let sessionId = $state(untrack(() => snapshot.session_id));
	let isDirty = $state(false);
	let hasRevisionConflict = $state(false);
	let isSaving = $state(false);
	let isSavingPassword = $state(false);
	let editVersion = $state(0);
	let passwordEditVersion = $state(0);
	let message = $state('');
	let errorMessage = $state('');
	let notices = $state<string[]>([]);
	let hasStoredPassword = $state(untrack(() => credentialPresence === 'stored'));

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
			isSavingPassword = false;
			hasRevisionConflict = false;
			passwordDraft = '';
			hasStoredPassword = credentialPresence === 'stored';
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

	$effect(() => {
		if (credentialPresence === 'stored') hasStoredPassword = true;
		if (credentialPresence === 'empty') hasStoredPassword = false;
	});

	function updateField(field: ConfigurationSnapshot['schema']['fields'][number], event: Event) {
		const target = event.currentTarget as HTMLInputElement;
		draft = {
			...draft,
			[field.id]:
				field.kind === 'toggle'
					? target.checked
					: field.kind === 'integer'
						? target.value === ''
							? ''
							: Number(target.value)
						: target.value
		};
		isDirty = true;
		editVersion += 1;
		message = '';
		errorMessage = '';
		notices = [];
	}

	async function savePassword() {
		if (isSavingPassword || !passwordDraft || !onSavePassword || snapshot.module !== 'obs') return;
		const requestSessionId = sessionId;
		const requestPassword = passwordDraft;
		const requestEditVersion = passwordEditVersion;
		isSavingPassword = true;
		message = '';
		errorMessage = '';
		try {
			const presence = await onSavePassword({ password: requestPassword });
			if (snapshot.session_id !== requestSessionId) return;
			if (presence !== 'stored')
				throw new DesktopRequestError({
					code: 'credential_unavailable',
					message: 'The OBS password could not be confirmed as stored.'
				});
			hasStoredPassword = true;
			if (passwordEditVersion === requestEditVersion) passwordDraft = '';
			message = 'OBS password saved.';
		} catch (error) {
			if (snapshot.session_id !== requestSessionId) return;
			errorMessage =
				error instanceof DesktopRequestError
					? error.message
					: 'The OBS password could not be saved.';
		} finally {
			if (snapshot.session_id === requestSessionId) isSavingPassword = false;
		}
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
	aria-busy={isSaving || isSavingPassword}
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
			{#each portableFields as field (field.id)}
				<div class="field-row">
					<div class="field-copy">
						<label for={`config-${field.id}`}>{field.label}</label>
						{#if field.description}<p>{field.description}</p>{/if}
					</div>
					{#if field.kind === 'toggle'}
						<input
							id={`config-${field.id}`}
							aria-label={field.label}
							type="checkbox"
							checked={Boolean(draft[field.id])}
							onchange={(event) => updateField(field, event)}
						/>
					{:else}
						<input
							id={`config-${field.id}`}
							aria-label={field.label}
							type={field.kind === 'integer' ? 'number' : 'text'}
							step={field.kind === 'integer' ? 1 : undefined}
							value={String(draft[field.id] ?? '')}
							required={field.required}
							onchange={(event) => updateField(field, event)}
						/>
					{/if}
				</div>
			{/each}
			{#if snapshot.module === 'obs' && onSavePassword}
				<div class="field-row password-row">
					<div class="field-copy">
						<label for="obs-password">OBS password</label>
						<p>
							{hasStoredPassword
								? 'A password is stored. Enter a new one to replace it.'
								: credentialPresence === 'unavailable'
									? 'Stored password status is unavailable. You can still try saving a password.'
									: 'Optional password for the OBS connection.'}
						</p>
					</div>
					<div class="password-controls">
						<input
							id="obs-password"
							type="password"
							autocomplete="new-password"
							value={passwordDraft}
							onchange={(event) => {
								passwordDraft = event.currentTarget.value;
								passwordEditVersion += 1;
								message = '';
								errorMessage = '';
								notices = [];
							}}
						/>
						<button
							type="button"
							onclick={savePassword}
							disabled={!passwordDraft || isSavingPassword}
						>
							{isSavingPassword ? 'Saving password…' : 'Save password'}
						</button>
					</div>
				</div>
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
					disabled={isSaving || isSavingPassword}>{backLabel}</button
				>{/if}
			<button type="submit" disabled={isSaving || hasRevisionConflict}
				>{isSaving ? 'Saving…' : 'Save settings'}</button
			>
		</footer>
	</form>
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
	.field-copy p {
		margin: 4px 0 0;
		color: var(--muted);
	}
	.configuration-fields {
		display: flex;
		flex-direction: column;
		gap: 0;
		padding: 0 16px;
		background: var(--side);
	}
	.field-row {
		display: grid;
		grid-template-columns: minmax(180px, 1fr) minmax(180px, 1fr);
		gap: 16px;
		align-items: center;
		min-height: 48px;
		padding: 8px 0;
		border-bottom: 1px solid var(--border);
	}
	.field-row:last-child {
		border-bottom: 0;
	}
	.field-copy label {
		font-weight: 400;
	}
	.field-copy p {
		font-size: 12px;
	}
	input:not([type='checkbox']) {
		width: 100%;
		height: 32px;
		padding: 4px 8px;
		background: var(--surface);
		border: 1px solid var(--border);
		border-radius: 4px;
	}
	input[type='checkbox'] {
		justify-self: start;
		width: 18px;
		height: 18px;
	}
	.password-controls {
		display: flex;
		gap: 8px;
		align-items: center;
	}
	.password-controls input {
		flex: 1;
		min-width: 0;
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
		.field-row {
			grid-template-columns: 1fr;
			gap: 8px;
		}
		.configuration-header {
			align-items: flex-start;
		}
	}
</style>
