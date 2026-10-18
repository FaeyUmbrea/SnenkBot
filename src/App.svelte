<script lang="ts">
	import { onMount, untrack, tick } from 'svelte';
	import { emit } from '@tauri-apps/api/event';
	import { createApplicationController } from './lib/application';
	import { createDesktopClient } from './lib/desktop';
	import DesktopShell from './lib/DesktopShell.svelte';
	import HomeView from './lib/HomeView.svelte';
	import WorkflowLibraryView from './lib/WorkflowLibraryView.svelte';
	import EventsView from './lib/EventsView.svelte';
	import EventTestDialog from './lib/EventTestDialog.svelte';
	let testingEvents = $state(false);
	import HistoryView from './lib/HistoryView.svelte';
	import SettingsView from './lib/SettingsView.svelte';
	import { importSupportHistory } from './lib/event-import';
	import ConfigurationImport from './lib/ConfigurationImport.svelte';
	import ConfigurationDirectory from './lib/ConfigurationDirectory.svelte';
	import IntegrationOverview from './lib/IntegrationOverview.svelte';
	import AccountConnectionsView from './lib/AccountConnectionsView.svelte';
	import WorkflowInputDialog from './lib/WorkflowInputDialog.svelte';
	import EditorView from './lib/EditorView.svelte';
	import { desktopNavigation, type DesktopPage } from './lib/navigation';
	import './lib/theme.css';
	import type {
		ConfigurationCatalogEntry,
		ConnectionStatus,
		SaveConfiguration
	} from './lib/contracts';
	let {
		client = createDesktopClient(),
		controller = createApplicationController(client)
	}: {
		client?: ReturnType<typeof createDesktopClient>;
		controller?: ReturnType<typeof createApplicationController>;
	} = $props();
	const application = untrack(() => controller);
	let selectedAccountRole = $state<string | undefined>(undefined);
	/** Save through the existing session and refresh the configured-entry directory. */
	async function saveConfiguration(request: SaveConfiguration) {
		const result = await controller.saveConfiguration(request);
		await controller.refreshCatalog().catch(() => undefined);
		return result;
	}
	/** Open one native catalog entry in its existing configuration or account editor. */
	function selectConfiguration(entry: ConfigurationCatalogEntry) {
		if (entry.module) quiet(controller.openSettings(entry.module));
		else {
			selectedAccountRole = undefined;
			controller.navigate('accounts');
		}
	}
	/** Open the selected connection while preserving its account role. */
	function configureConnection(connection: ConnectionStatus) {
		selectedAccountRole =
			connection.integration === $application.accounts?.integration_id
				? $application.accounts.roles.find(
						({ descriptor }) => descriptor.id === connection.connection
					)?.descriptor.id
				: undefined;
		quiet(controller.configure(connection));
	}
	/** Start only an account role declared by the native integration. */
	async function startAccountLogin(id: string) {
		const role = $application.accounts?.roles.find(({ descriptor }) => descriptor.id === id)
			?.descriptor.id;
		if (!role) throw new Error('This account role is unavailable.');
		return controller.startTwitchLogin({ role });
	}

	const accountEntry = $derived(
		$application.catalog.find((entry) => entry.id === $application.accounts?.integration_id) ??
			$application.catalog.find((entry) => !entry.module)
	);
	const workflows = $derived($application.snapshot?.workflows ?? []);
	const connections = $derived($application.snapshot?.connections ?? []);
	const titles = $derived(Object.fromEntries(workflows.map((w) => [w.id, w.title])));
	const title = $derived(
		$application.page === 'automations' && $application.editor
			? ($application.editor.draft.name ?? titles[$application.workflowId ?? ''] ?? 'Automation')
			: (($application.page === 'general' ? 'General' : $application.configuration?.schema.title) ??
					($application.page === 'accounts'
						? ($application.accounts?.title ?? accountEntry?.title ?? 'Accounts')
						: (desktopNavigation.find((p) => p.id === $application.page)?.title ?? 'SnenkBot')))
	);
	/** Retry a failed settings read without restarting unrelated application services. */
	function retryRequest() {
		const module = $application.configurationModule;
		quiet(
			module && !$application.configuration
				? controller.openSettings(module)
				: controller.retryStartup()
		);
	}
	function quiet(operation: Promise<unknown>) {
		void operation.catch(() => undefined);
	}
	function navigate(page: DesktopPage) {
		if (page === 'automations') controller.showLibrary();
		else if (page === 'history') controller.openHistory();
		else if (page === 'general') {
			controller.navigate(page);
			const entries = $application.catalog.filter((entry) => entry.category === page);
			if (entries.length === 1) selectConfiguration(entries[0]);
			else quiet(controller.refreshCatalog());
		} else {
			controller.navigate(page);
			if (['broadcast', 'streaming', 'devices'].includes(page)) quiet(controller.refreshCatalog());
		}
	}
	onMount(() => {
		void controller.initialize().then(async () => {
			await tick();
			try {
				await emit('frontend-ready', {
					startup: $application.snapshot?.startup.state ?? 'failed',
					error: $application.error
				});
			} catch {
				/* Browser fixtures have no native event transport. */
			}
		});
		return () => {
			void controller.dispose();
		};
	});
</script>

<DesktopShell
	page={$application.page === 'accounts' ? 'streaming' : $application.page}
	{title}
	{connections}
	requests={$application.snapshot?.reconfiguration_requests ?? []}
	errors={$application.snapshot?.errors ?? []}
	onNavigate={navigate}
	onConfigure={configureConnection}
	onDismissError={(id) => controller.dismissError({ id })}
	onBack={$application.returnRoutes.length
		? controller.returnToContext
		: $application.page === 'automations' && $application.workflowId
			? controller.showLibrary
			: undefined}
>
	{#if $application.error}<div class="application-error" role="alert">
			<span>{$application.error}</span><button onclick={controller.clearError}>Dismiss</button
			><button onclick={retryRequest}>Retry</button>
		</div>{/if}
	{#if !$application.snapshot}<p class="loading">Starting SnenkBot…</p>
	{:else if $application.snapshot.startup.state !== 'ready'}<div class="loading">
			<h2>SnenkBot could not start</h2>
			<p>Check the application notice, then retry.</p>
			<button onclick={() => quiet(controller.retryStartup())}>Retry startup</button>
		</div>
	{:else}
		{#if $application.configurationModule && !$application.configuration}
			<section class="page-view settings-page" aria-label="Settings loading">
				{#if $application.loading}<p role="status">Loading settings…</p>
				{:else}<h2>Settings could not be opened</h2>
					<p>Your saved configuration has not been changed.</p>
					<button onclick={retryRequest}>Retry settings</button>
				{/if}
			</section>
		{/if}
		<div class="page-view" hidden={$application.page !== 'home'}>
			<HomeView
				{connections}
				{workflows}
				history={$application.recentHistory}
				historyLoading={$application.recentHistoryLoading}
				historyError={$application.recentHistoryError ?? ''}
				onConfigure={configureConnection}
				onNavigate={navigate}
				onOpen={(id) => quiet(controller.openWorkflow(id))}
				onRun={controller.run}
				onLibrary={controller.showLibrary}
				onHistory={controller.openHistory}
				onRefreshHistory={controller.refreshRecentHistory}
			/>
		</div>
		<div
			class="page-view"
			class:editor-page={!!$application.workflowId}
			hidden={$application.page !== 'automations'}
		>
			{#if $application.workflowId}{#if $application.editor}<EditorView {controller} />{:else}<p
						class="loading"
					>
						Opening automation…
					</p>{/if}{:else}<WorkflowLibraryView
					{workflows}
					query={$application.views.automations?.search ?? ''}
					onQueryChange={(q) => controller.setSearch(q, 'automations')}
					onOpen={(id) => quiet(controller.openWorkflow(id))}
					onRun={controller.run}
					onCreate={controller.createWorkflow}
					disabled={$application.loading}
				/>{/if}
		</div>
		{#if $application.page === 'history'}<div class="page-view editor-page">
				<HistoryView
					loadPage={client.historyPage}
					inspectRun={client.inspectHistory}
					workflowTitles={titles}
					schemas={$application.schemas}
					requestedRunId={$application.historyRunId}
					onSelectRun={controller.selectHistoryRun}
				/>
			</div>{/if}
		<div class="page-view settings-page" hidden={$application.page !== 'accounts'}>
			{#if $application.accounts}<AccountConnectionsView
					showTitle={false}
					accounts={$application.accounts}
					selectedRole={selectedAccountRole}
					authentication={$application.authentication}
					{connections}
					requests={$application.snapshot.reconfiguration_requests}
					onStart={startAccountLogin}
					onApprove={(attempt_id, user_id) =>
						controller.approveTwitchLogin({ attempt_id, user_id })}
					onCancel={(attempt_id) => controller.cancelTwitchLogin({ attempt_id })}
					onOpen={(attempt_id) => controller.openTwitchLogin({ attempt_id })}
					onCopy={(attempt_id, target) => controller.copyTwitchLogin({ attempt_id, target })}
				/>{:else}<div class="loading">
					<p>{$application.accountError ?? 'Loading accounts…'}</p>
					<button onclick={() => quiet(controller.refreshAccounts())}>Retry accounts</button>
				</div>{/if}
		</div>

		{#if $application.page === 'events'}<div class="page-view settings-page">
				<EventsView
					showTitle={false}
					load={client.eventsList}
					targets={client.eventsTargets}
					replay={client.eventsReplay}
				>
					{#snippet actions()}
						<ConfigurationImport
							label="Import history…"
							description="Import an existing events.json support history file."
							maxBytes={2 * 1024 * 1024}
							onImport={(value) => importSupportHistory(client, value)}
						/>
					{/snippet}
				</EventsView>
			</div>{/if}

		{#if ['general', 'broadcast', 'streaming', 'devices'].includes($application.page) && !$application.configurationModule}
			<div class="page-view settings-page">
				{#if $application.catalogError}<p role="alert">{$application.catalogError}</p>
					<button onclick={() => quiet(controller.refreshCatalog())}>Retry</button>{/if}
				{#key $application.page}<ConfigurationDirectory
						title={desktopNavigation.find((page) => page.id === $application.page)?.title ??
							'Settings'}
						entries={$application.catalog.filter((entry) => entry.category === $application.page)}
						onSelect={selectConfiguration}
					/>{/key}
			</div>
		{/if}
		{#each Object.values($application.configurations) as config (config.module)}
			{@const entry = $application.catalog.find((entry) => entry.module === config.module)}
			<div
				class="page-view settings-page"
				hidden={$application.configurationModule !== config.module}
			>
				<IntegrationOverview
					integration={entry?.id ?? config.module}
					{connections}
					{workflows}
					onOpen={(id) => quiet(controller.openWorkflow(id))}
				>
					{#key config.session_id}
						<SettingsView
							showTitle={false}
							snapshot={config}
							{client}
							onSave={saveConfiguration}
							onAction={(action) => {
								if (action.id === 'api.test_events') testingEvents = true;
							}}
							active={$application.configurationModule === config.module}
						/>
					{/key}
				</IntegrationOverview>
			</div>
		{/each}
	{/if}
</DesktopShell>
{#if testingEvents}<EventTestDialog
		open={testingEvents}
		load={client.eventTestOptions}
		choices={client.actionChoices}
		send={client.eventTestSend}
		onClose={() => (testingEvents = false)}
	/>{/if}
<WorkflowInputDialog
	request={$application.snapshot?.pending_input ?? null}
	onSubmit={controller.submitInput}
	onCancel={controller.cancelInput}
/>

<style>
	:global(html),
	:global(body),
	:global(#app) {
		margin: 0;
		width: 100%;
		height: 100%;
		overflow: hidden;
		background: #0b0808;
	}
	:global([hidden]) {
		display: none !important;
	}
	.page-view {
		flex: 1;
		min-width: 0;
		min-height: 0;
		overflow: auto;
	}
	.editor-page {
		overflow: hidden;
	}
	.settings-page {
		padding: 24px;
	}
	.application-error {
		flex: none;
		flex-wrap: wrap;
		display: flex;
		gap: 12px;
		padding: 12px 24px;
		background: var(--surface);
		align-items: center;
	}
	.application-error span {
		flex: 1;
	}
	.loading {
		padding: 24px;
	}
</style>
