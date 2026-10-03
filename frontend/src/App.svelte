<script lang="ts">
	import { onMount, untrack, tick } from 'svelte';
	import { emit } from '@tauri-apps/api/event';
	import { createApplicationController } from './lib/application';
	import { createDesktopClient } from './lib/desktop';
	import DesktopShell from './lib/DesktopShell.svelte';
	import HomeView from './lib/HomeView.svelte';
	import WorkflowLibraryView from './lib/WorkflowLibraryView.svelte';
	import HistoryView from './lib/HistoryView.svelte';
	import ObsConnectionView from './lib/ObsConnectionView.svelte';
	import VtubeConnectionView from './lib/VtubeConnectionView.svelte';
	import TwitchAccountsView from './lib/TwitchAccountsView.svelte';
	import WorkflowInputDialog from './lib/WorkflowInputDialog.svelte';
	import EditorView from './lib/EditorView.svelte';
	import { desktopNavigation, type DesktopPage } from './lib/navigation';
	import './lib/theme.css';
	let {
		client = createDesktopClient(),
		controller = createApplicationController(client)
	}: {
		client?: ReturnType<typeof createDesktopClient>;
		controller?: ReturnType<typeof createApplicationController>;
	} = $props();
	const application = untrack(() => controller);
	const workflows = $derived($application.snapshot?.workflows ?? []);
	const connections = $derived($application.snapshot?.connections ?? []);
	const titles = $derived(Object.fromEntries(workflows.map((w) => [w.id, w.title])));
	const title = $derived(
		$application.page === 'automations' && $application.editor
			? ($application.editor.draft.name ?? titles[$application.workflowId ?? ''] ?? 'Automation')
			: (desktopNavigation.find((p) => p.id === $application.page)?.title ?? 'SnenkBot')
	);
	function quiet(operation: Promise<unknown>) {
		void operation.catch(() => undefined);
	}
	function navigate(page: DesktopPage) {
		if (page === 'automations') controller.showLibrary();
		else if (page === 'broadcast') quiet(controller.openSettings('obs'));
		else if (page === 'devices') quiet(controller.openSettings('vtube_studio'));
		else if (page === 'history') controller.openHistory();
		else controller.navigate(page);
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
	page={$application.page}
	{title}
	{connections}
	requests={$application.snapshot?.reconfiguration_requests ?? []}
	errors={$application.snapshot?.errors ?? []}
	onNavigate={navigate}
	onConfigure={(connection) => quiet(controller.configure(connection))}
	onDismissError={(id) => controller.dismissError({ id })}
	onBack={$application.returnRoutes.length
		? controller.returnToContext
		: $application.page === 'automations' && $application.workflowId
			? controller.showLibrary
			: undefined}
>
	{#if $application.error}<div class="application-error" role="alert">
			<span>{$application.error}</span><button onclick={controller.clearError}>Dismiss</button
			><button onclick={() => quiet(controller.retryStartup())}>Retry</button>
		</div>{/if}
	{#if !$application.snapshot}<p class="loading">Starting SnenkBot…</p>
	{:else if $application.snapshot.startup.state !== 'ready'}<div class="loading">
			<h2>SnenkBot could not start</h2>
			<p>Check the application notice, then retry.</p>
			<button onclick={() => quiet(controller.retryStartup())}>Retry startup</button>
		</div>
	{:else}
		<div class="page-view" hidden={$application.page !== 'home'}>
			<HomeView
				{connections}
				{workflows}
				history={$application.recentHistory}
				historyLoading={$application.recentHistoryLoading}
				historyError={$application.recentHistoryError ?? ''}
				onConfigure={(c) => quiet(controller.configure(c))}
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
		{#if $application.configurations.obs}<div
				class="page-view settings-page"
				hidden={$application.configurationModule !== 'obs'}
			>
				<ObsConnectionView
					snapshot={$application.configurations.obs}
					{connections}
					{workflows}
					onOpen={(id) => quiet(controller.openWorkflow(id))}
					onSave={controller.saveConfiguration}
					onPresence={client.obsPasswordStatus}
					onSavePassword={client.saveObsPassword}
				/>
			</div>{/if}
		{#if $application.configurations.vtube_studio}<div
				class="page-view settings-page"
				hidden={$application.configurationModule !== 'vtube_studio'}
			>
				<VtubeConnectionView
					snapshot={$application.configurations.vtube_studio}
					{connections}
					{workflows}
					onOpen={(id) => quiet(controller.openWorkflow(id))}
					onSave={controller.saveConfiguration}
					onPresence={client.vtubeAuthorizationStatus}
					onAuthorize={client.authorizeVtube}
					onForget={client.forgetVtubeAuthorization}
				/>
			</div>{/if}
		<div class="page-view settings-page" hidden={$application.page !== 'streaming'}>
			{#if $application.accounts}<TwitchAccountsView
					accounts={$application.accounts}
					authentication={$application.authentication}
					{connections}
					requests={$application.snapshot.reconfiguration_requests}
					onStart={controller.startTwitchLogin}
					onApprove={controller.approveTwitchLogin}
					onCancel={controller.cancelTwitchLogin}
					onOpen={controller.openTwitchLogin}
					onCopy={controller.copyTwitchLogin}
				/>{:else}<div class="loading">
					<p>{$application.accountError ?? 'Loading accounts…'}</p>
					<button onclick={() => quiet(controller.refreshAccounts())}>Retry accounts</button>
				</div>{/if}
		</div>
		{#if $application.page === 'general'}<div class="general">
				<h2>General</h2>
				<p>
					SnenkBot connects configured integrations when it starts. Automations and saved versions
					are stored locally.
				</p>
				<div class="general-actions">
					<button onclick={() => navigate('broadcast')}>Configure OBS</button><button
						onclick={() => navigate('streaming')}>Configure Twitch</button
					><button onclick={() => navigate('devices')}>Configure VTube Studio</button>
				</div>
			</div>{/if}
	{/if}
</DesktopShell>
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
	.loading,
	.general {
		padding: 24px;
	}
	.general h2 {
		margin: 0;
		font-size: 16px;
	}
	.general p {
		max-width: 600px;
		line-height: 1.5;
		color: var(--muted);
	}
	.general-actions {
		display: flex;
		flex-wrap: wrap;
		gap: 12px;
	}
	.general button {
		min-height: 32px;
		padding: 6px 12px;
		border: 1px solid var(--border);
		border-radius: 6px;
		background: var(--surface);
	}
</style>
