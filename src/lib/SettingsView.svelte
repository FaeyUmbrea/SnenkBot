<script lang="ts">
	import { untrack } from 'svelte';
	import ConfigurationView from './ConfigurationView.svelte';
	import ConfigurationCollection from './ConfigurationCollection.svelte';
	import SectionNavigation from './SectionNavigation.svelte';
	import { createConfigurationCollection } from './configuration-collection';
	import type { createDesktopClient } from './desktop';
	import type {
		ConfigurationActionDescriptor,
		ConfigurationSnapshot,
		ConfigurationSaveResult,
		SaveConfiguration
	} from './contracts';

	let {
		snapshot,
		client,
		onSave,
		onBack,
		backLabel = 'Back',
		active = true,
		showTitle = true,
		onAction
	}: {
		snapshot: ConfigurationSnapshot;
		client: ReturnType<typeof createDesktopClient>;
		onSave: (request: SaveConfiguration) => Promise<ConfigurationSaveResult>;
		onBack?: () => void;
		backLabel?: string;
		active?: boolean;
		showTitle?: boolean;
		onAction?: (action: ConfigurationActionDescriptor) => void;
	} = $props();
	const records = untrack(() =>
		snapshot.collection ? createConfigurationCollection(client, snapshot) : null
	);
	let section = $state('settings');
	let recordsOpened = $state(false);
	const credential = $derived(snapshot.credential);
	const authorization = $derived(snapshot.authorization);
</script>

{#if snapshot.collection}
	<SectionNavigation
		label={`${snapshot.schema.title} sections`}
		items={[
			{ id: 'settings', label: snapshot.schema.title },
			{ id: 'records', label: snapshot.collection.title }
		]}
		value={section}
		onChange={(value) => {
			section = value;
			if (value === 'records') recordsOpened = true;
		}}
	/>
{/if}
<div hidden={section !== 'settings'}>
	<ConfigurationView
		{snapshot}
		{showTitle}
		{onSave}
		{onBack}
		{backLabel}
		pickDirectory={(field_path, current_path) =>
			client.pickConfigurationDirectory({
				session_id: snapshot.session_id,
				collection_id: null,
				field_path,
				current_path
			})}
		credential={credential ?? undefined}
		onRefreshCredential={credential
			? () =>
					client.configurationCredentialStatus({
						session_id: snapshot.session_id,
						credential_id: credential.id
					})
			: undefined}
		onSaveCredential={credential
			? (value) =>
					client.saveConfigurationCredential({
						session_id: snapshot.session_id,
						credential_id: credential.id,
						value
					})
			: undefined}
		onAuthorizationPresence={authorization
			? () =>
					client.configurationAuthorizationStatus({
						session_id: snapshot.session_id,
						authorization_id: authorization.id
					})
			: undefined}
		onAuthorize={authorization
			? () =>
					client.configurationAuthorize({
						session_id: snapshot.session_id,
						authorization_id: authorization.id
					})
			: undefined}
		onForgetAuthorization={authorization
			? () =>
					client.configurationForgetAuthorization({
						session_id: snapshot.session_id,
						authorization_id: authorization.id
					})
			: undefined}
	>
		{#snippet actions()}
			{#if onAction}
				{#each snapshot.actions as action (action.id)}
					<button
						type="button"
						class="field-button"
						title={action.description}
						onclick={() => onAction?.(action)}>{action.label}</button
					>
				{/each}
			{/if}
		{/snippet}
	</ConfigurationView>
</div>
{#if recordsOpened && records && snapshot.collection}
	<div hidden={section !== 'records'}>
		<ConfigurationCollection
			title={snapshot.collection.title}
			itemLabel={snapshot.collection.item_label}
			description={snapshot.collection.description}
			{...records}
			active={active && section === 'records'}
		/>
	</div>
{/if}
