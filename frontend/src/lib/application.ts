import { createEditorResourceController } from './editor-resources';
import { writable } from 'svelte/store';
import { allSteps } from './canvas';
import { createDesktopClient, DesktopRequestError } from './desktop';
import {
	subscribeDesktopUpdates,
	type DesktopFeed,
	type DesktopFeedCallbacks
} from './desktop-feed';
import type { DesktopPage } from './navigation';
import type {
	ActionDefinition,
	HistoryPage,
	ConfigSchema,
	ConfigurationSnapshot,
	ConnectionModule,
	ConnectionStatus,
	DesktopSnapshot,
	EditorOperation,
	EditorSnapshot_Serialize,
	SaveConfiguration,
	TwitchAccountsSnapshot,
	TwitchAuthentication,
	StartTwitchLogin
} from './contracts/index';

export type DesktopClient = ReturnType<typeof createDesktopClient>;
export type DesktopFeedFactory = (callbacks: DesktopFeedCallbacks) => DesktopFeed;
export interface ApplicationRoute {
	page: DesktopPage;
	workflowId: string | null;
	configurationModule: ConnectionModule | null;
	historyRunId: string | null;
}
export interface ApplicationView {
	search: string;
	scroll: number;
}
export interface ApplicationState extends ApplicationRoute {
	snapshot: DesktopSnapshot | null;
	schemas: ConfigSchema[];
	definitions: ActionDefinition[];
	accounts: TwitchAccountsSnapshot | null;
	accountLoading: boolean;
	accountError: string | null;
	recentHistory: HistoryPage | null;
	recentHistoryLoading: boolean;
	recentHistoryError: string | null;
	authentication: TwitchAuthentication | null;
	returnRoutes: ApplicationRoute[];
	editor: EditorSnapshot_Serialize | null;
	editors: Record<string, EditorSnapshot_Serialize>;
	selectedStepId: string | null;
	configuration: ConfigurationSnapshot | null;
	configurations: Partial<Record<ConnectionModule, ConfigurationSnapshot>>;
	views: Record<string, ApplicationView>;
	loading: boolean;
	error: string | null;
}

/** Owns native subscriptions and sessions for the lifetime of the mounted application. */
export function createApplicationController(
	client: DesktopClient = createDesktopClient(),
	feedFactory: DesktopFeedFactory = subscribeDesktopUpdates
) {
	const resources = createEditorResourceController(client);
	let state: ApplicationState = {
		page: 'home',
		historyRunId: null,
		workflowId: null,
		configurationModule: null,
		snapshot: null,
		schemas: [],
		definitions: [],
		accounts: null,
		accountLoading: false,
		accountError: null,
		recentHistory: null,
		recentHistoryLoading: false,
		recentHistoryError: null,
		authentication: null,
		returnRoutes: [],
		editor: null,
		editors: {},
		selectedStepId: null,
		configuration: null,
		configurations: {},
		views: {},
		loading: false,
		error: null
	};
	const store = writable(state);
	let disposed = false;
	let feed: DesktopFeed | undefined;
	let feedFailed = false;
	let initialization: Promise<void> | undefined;
	let disposal: Promise<void> | undefined;
	let bootstrapped = false;
	let bootstrapPromise: Promise<void> | undefined;
	let retryPromise: Promise<void> | undefined;
	let schemasLoaded = false;
	let authenticationLoaded = false;
	let accountGeneration = 0;
	let authGeneration = 0;
	let navigationGeneration = 0;
	let pendingCount = 0;
	let lastConnectedAttempt: string | undefined;
	let lastCompletedRun: string | undefined;
	let historyPromise: Promise<void> | undefined;
	let historyInvalidated = false;
	const selectedSteps = new Map<string, string | null>();
	const editorOpens = new Map<string, Promise<EditorSnapshot_Serialize>>();
	const configurationOpens = new Map<ConnectionModule, Promise<ConfigurationSnapshot>>();
	const queues = new Map<string, Promise<unknown>>();
	const ownedEditors = new Set<string>();
	const ownedConfigurations = new Set<string>();

	function publish(change: Partial<ApplicationState>) {
		if (disposed) return;
		state = { ...state, ...change };
		resources.setContext(
			state.editor && state.selectedStepId
				? {
						session_id: state.editor.session_id,
						expected_revision: state.editor.revision,
						step_id: state.selectedStepId
					}
				: null
		);
		store.set(state);
	}
	function message(error: unknown) {
		return error instanceof DesktopRequestError
			? error.message
			: 'The application could not complete this request.';
	}
	async function request<T>(operation: () => Promise<T>): Promise<T> {
		if (disposed) throw new Error('The application has closed.');
		pendingCount += 1;
		publish({ loading: true, error: null });
		try {
			if (disposed) throw new Error('The application has closed.');
			return await operation();
		} catch (error) {
			publish({ error: message(error) });
			throw error;
		} finally {
			pendingCount -= 1;
			publish({ loading: pendingCount > 0 });
		}
	}
	function enqueue<T>(sessionId: string, operation: () => Promise<T>): Promise<T> {
		const previous = queues.get(sessionId) ?? Promise.resolve();
		const pending = previous.catch(() => undefined).then(() => request(operation));
		queues.set(sessionId, pending);
		void pending
			.finally(() => {
				if (queues.get(sessionId) === pending) queues.delete(sessionId);
			})
			.catch(() => undefined);
		return pending;
	}
	async function refreshAccounts() {
		if (disposed) return;
		const generation = ++accountGeneration;
		publish({ accountLoading: true, accountError: null });
		try {
			const accounts = await client.twitchAccounts();
			if (generation === accountGeneration) publish({ accounts });
		} catch (error) {
			if (generation === accountGeneration) publish({ accountError: message(error) });
		} finally {
			if (generation === accountGeneration) publish({ accountLoading: false });
		}
	}
	/** Coalesces completion bursts into one follow-up read; never polls history. */
	function refreshRecentHistory(): Promise<void> {
		if (disposed) return Promise.resolve();
		if (historyPromise) return historyPromise;
		publish({ recentHistoryLoading: true, recentHistoryError: null });
		historyPromise = (async () => {
			do {
				historyInvalidated = false;
				try {
					if (disposed) return;
					const recentHistory = await client.historyPage({
						page_size: 6,
						cursor: null,
						filter: { workflow_id: null, outcome: null, trigger_kind: null }
					});
					publish({ recentHistory, recentHistoryError: null });
				} catch (error) {
					publish({ recentHistoryError: message(error) });
				}
			} while (historyInvalidated && !disposed);
		})().finally(() => {
			historyPromise = undefined;
			publish({ recentHistoryLoading: false });
		});
		return historyPromise;
	}
	function observeAuthentication(authentication: TwitchAuthentication | null) {
		authGeneration += 1;
		publish({ authentication });
		if (
			authentication?.phase.state !== 'connected' ||
			lastConnectedAttempt === authentication.attempt_id
		)
			return;
		lastConnectedAttempt = authentication.attempt_id;
		if (bootstrapped && state.snapshot?.startup.state === 'ready') void refreshAccounts();
	}
	function bootstrap(): Promise<void> {
		if (bootstrapPromise) return bootstrapPromise;
		if (bootstrapped || disposed || state.snapshot?.startup.state !== 'ready')
			return Promise.resolve();
		bootstrapped = true;
		const generation = authGeneration;
		bootstrapPromise = Promise.all([
			refreshAccounts(),
			refreshRecentHistory(),
			schemasLoaded
				? Promise.resolve()
				: client
						.actionDefinitions()
						.then((definitions) => {
							schemasLoaded = true;
							publish({ definitions, schemas: definitions.map((definition) => definition.schema) });
						})
						.catch((error: unknown) => publish({ error: message(error) })),
			authenticationLoaded
				? Promise.resolve()
				: client
						.twitchLoginSnapshot()
						.then((authentication) => {
							authenticationLoaded = true;
							if (!disposed && generation === authGeneration && !state.authentication)
								observeAuthentication(authentication);
						})
						.catch((error: unknown) => publish({ error: message(error) }))
		]).then(() => undefined);
		return bootstrapPromise;
	}
	function attachFeed(): Promise<void> {
		feedFailed = false;
		// Feed factories may synchronously publish their first snapshot before returning.
		try {
			feed = feedFactory({
				onSnapshot(snapshot) {
					if (disposed) return;
					const previousAuth = state.snapshot?.twitch_authentication;
					publish({ snapshot });
					if (snapshot.twitch_authentication !== previousAuth)
						observeAuthentication(snapshot.twitch_authentication);
					const completedRun = snapshot.last_completed?.run_id;
					if (completedRun && completedRun !== lastCompletedRun) {
						lastCompletedRun = completedRun;
						if (bootstrapped && snapshot.startup.state === 'ready') {
							if (historyPromise) historyInvalidated = true;
							else void refreshRecentHistory();
						}
					}
					void bootstrap();
				},
				onError: (error) => publish({ error: message(error) })
			});
			if (disposed) feed.dispose();
			return feed.ready.then(bootstrap).catch((error: unknown) => {
				feedFailed = true;
				feed?.dispose();
				feed = undefined;
				publish({ error: message(error) });
			});
		} catch (error) {
			feedFailed = true;
			publish({ error: message(error) });
			return Promise.resolve();
		}
	}
	function initialize(): Promise<void> {
		if (initialization) return initialization;
		if (disposed) return Promise.resolve();
		initialization = Promise.resolve();
		initialization = attachFeed();
		return initialization;
	}
	function retryStartup(): Promise<void> {
		if (disposed) return Promise.resolve();
		if (retryPromise) return retryPromise;
		retryPromise = (async () => {
			await initialize();
			await bootstrapPromise;
			if (disposed) return;
			publish({ error: null });
			try {
				if (feedFailed) {
					await attachFeed();
					if (feedFailed) return;
				} else {
					await feed?.refresh();
				}
				if (disposed) return;
				bootstrapped = false;
				bootstrapPromise = undefined;
				await bootstrap();
			} catch (error) {
				publish({ error: message(error) });
			}
		})();
		void retryPromise
			.finally(() => {
				retryPromise = undefined;
			})
			.catch(() => undefined);
		return retryPromise;
	}
	function currentRoute(): ApplicationRoute {
		return {
			page: state.page,
			historyRunId: state.historyRunId,
			workflowId: state.workflowId,
			configurationModule: state.configurationModule
		};
	}
	function rememberContext() {
		publish({ returnRoutes: [...state.returnRoutes, currentRoute()].slice(-64) });
	}
	function showRoute(
		route: Omit<ApplicationRoute, 'historyRunId'> & { historyRunId?: string | null }
	) {
		navigationGeneration += 1;
		publish({
			...route,
			historyRunId: route.historyRunId ?? null,
			editor: route.workflowId ? (state.editors[route.workflowId] ?? null) : null,
			selectedStepId: route.workflowId ? (selectedSteps.get(route.workflowId) ?? null) : null,
			configuration: route.configurationModule
				? (state.configurations[route.configurationModule] ?? null)
				: null
		});
	}
	function navigate(page: DesktopPage) {
		showRoute({ page, workflowId: state.workflowId, configurationModule: null });
	}
	function openHistory(runId: string | null = null) {
		if (disposed) return;
		if (state.page !== 'history') rememberContext();
		showRoute({
			page: 'history',
			workflowId: state.workflowId,
			configurationModule: null,
			historyRunId: runId
		});
	}
	function showLibrary() {
		showRoute({ page: 'automations', workflowId: null, configurationModule: null });
	}
	function cacheEditor(workflowId: string, editor: EditorSnapshot_Serialize) {
		const previous = state.editors[workflowId];
		if (
			previous &&
			previous.session_id === editor.session_id &&
			previous.revision > editor.revision
		)
			return;
		const selectedStepId = selectedSteps.get(workflowId) ?? null;
		const selection =
			selectedStepId &&
			!allSteps(editor.draft.workflow.steps).some((step) => step.id === selectedStepId)
				? null
				: selectedStepId;
		selectedSteps.set(workflowId, selection);
		publish({
			editors: { ...state.editors, [workflowId]: editor },
			...(state.workflowId === workflowId ? { editor, selectedStepId: selection } : {})
		});
	}
	async function openWorkflow(workflowId: string) {
		if (disposed) return;
		showRoute({ page: 'automations', workflowId, configurationModule: null });
		const generation = navigationGeneration;
		if (state.editors[workflowId]) return;
		let pending = editorOpens.get(workflowId);
		if (!pending) {
			pending = request(() => client.openEditor({ workflow_id: workflowId })).then((editor) => {
				ownedEditors.add(editor.session_id);
				cacheEditor(workflowId, editor);
				return editor;
			});
			editorOpens.set(workflowId, pending);
			void pending.finally(() => editorOpens.delete(workflowId)).catch(() => undefined);
		}
		try {
			const editor = await pending;
			if (generation === navigationGeneration) publish({ editor });
		} catch {
			/* The request surfaced the error; preserve the current route and drafts. */
		}
	}
	function selectStep(stepId: string | null) {
		if (!state.workflowId || disposed) return;
		selectedSteps.set(state.workflowId, stepId);
		publish({ selectedStepId: stepId });
	}
	function cacheConfiguration(configuration: ConfigurationSnapshot) {
		const previous = state.configurations[configuration.module];
		if (
			previous &&
			previous.session_id === configuration.session_id &&
			previous.revision > configuration.revision
		)
			return;
		publish({
			configurations: { ...state.configurations, [configuration.module]: configuration },
			...(state.configurationModule === configuration.module ? { configuration } : {})
		});
	}
	async function openSettings(module: ConnectionModule) {
		if (disposed) return;
		const route = currentRoute();
		if (route.configurationModule !== module) rememberContext();
		showRoute({
			page: module === 'obs' ? 'broadcast' : 'devices',
			workflowId: state.workflowId,
			configurationModule: module
		});
		if (state.configurations[module]) return;
		let pending = configurationOpens.get(module);
		if (!pending) {
			pending = request(() => client.openConfiguration({ module })).then((configuration) => {
				ownedConfigurations.add(configuration.session_id);
				cacheConfiguration(configuration);
				return configuration;
			});
			configurationOpens.set(module, pending);
			void pending.finally(() => configurationOpens.delete(module)).catch(() => undefined);
		}
		try {
			await pending;
		} catch {
			/* Request errors preserve cached sessions. */
		}
	}
	function configure(connection: ConnectionStatus) {
		if (disposed) return Promise.resolve();
		if (connection.integration === 'obs' || connection.integration === 'vtube_studio')
			return openSettings(connection.integration);
		if (state.page !== 'streaming' || state.configurationModule)
			if (state.page !== 'streaming') rememberContext();
		navigate('streaming');
		return Promise.resolve();
	}
	function returnToContext() {
		const routes = state.returnRoutes.slice();
		const route = routes.pop();
		if (!route) return;
		publish({ returnRoutes: routes });
		showRoute(route);
	}
	function viewKey() {
		return state.configurationModule
			? `configuration:${state.configurationModule}`
			: state.page === 'automations' && state.workflowId
				? `workflow:${state.workflowId}`
				: state.page;
	}
	function updateView(change: Partial<ApplicationView>, key = viewKey()) {
		publish({
			views: {
				...state.views,
				[key]: { ...(state.views[key] ?? { search: '', scroll: 0 }), ...change }
			}
		});
	}
	function edit(operation: EditorOperation) {
		const workflowId = state.workflowId;
		const editor = state.editor;
		if (!workflowId || !editor) return Promise.resolve(null);
		return enqueue(editor.session_id, async () => {
			const updated = await client.applyEditorEdit({
				session_id: editor.session_id,
				expected_revision: state.editors[workflowId].revision,
				edit: operation
			});
			cacheEditor(workflowId, updated);
			return updated;
		});
	}
	function saveEditor() {
		const workflowId = state.workflowId;
		const editor = state.editor;
		if (!workflowId || !editor) return Promise.resolve(null);
		return enqueue(editor.session_id, async () => {
			const result = await client.saveEditor({
				session_id: editor.session_id,
				expected_revision: state.editors[workflowId].revision
			});
			cacheEditor(workflowId, result.snapshot);
			return result;
		});
	}
	function saveConfiguration(data: SaveConfiguration) {
		return enqueue(data.session_id, async () => {
			const result = await client.saveConfiguration(data);
			cacheConfiguration(result.snapshot);
			return result;
		});
	}
	async function startTwitchLogin(data: StartTwitchLogin) {
		const generation = authGeneration;
		try {
			const result = await request(() => client.startTwitchLogin(data));
			if (!disposed && generation === authGeneration) observeAuthentication(result);
			return state.authentication ?? result;
		} catch (error) {
			if (!disposed) {
				try {
					const authentication = await client.twitchLoginSnapshot();
					if (generation === authGeneration) observeAuthentication(authentication);
				} catch {
					/* Keep the feed's most recent authentication state. */
				}
			}
			throw error;
		}
	}
	function dispose(): Promise<void> {
		if (disposal) return disposal;
		disposed = true;
		resources.dispose();
		feed?.dispose();
		disposal = (async () => {
			await Promise.allSettled([
				...editorOpens.values(),
				...configurationOpens.values(),
				...queues.values()
			]);
			await Promise.allSettled([
				...Array.from(ownedEditors, (session_id) => client.closeEditor({ session_id })),
				...Array.from(ownedConfigurations, (session_id) =>
					client.closeConfiguration({ session_id })
				)
			]);
			ownedEditors.clear();
			ownedConfigurations.clear();
		})();
		return disposal;
	}
	return {
		subscribe: store.subscribe,
		resources,
		showLibrary,
		openHistory,
		selectHistoryRun: (historyRunId: string | null) => publish({ historyRunId }),
		retryStartup,
		createWorkflow: async (name: string) => {
			const generation = navigationGeneration;
			const workflowId = await request(() => client.createWorkflow({ name }));
			if (generation === navigationGeneration) await openWorkflow(workflowId);
			return workflowId;
		},
		initialize,
		dispose,
		navigate,
		openWorkflow,
		selectStep,
		configure,
		openSettings,
		returnToContext,
		edit,
		saveEditor,
		saveConfiguration,
		setSearch: (search: string, key?: string) => updateView({ search }, key),
		setScroll: (scroll: number, key?: string) => updateView({ scroll }, key),
		viewKey,
		refreshAccounts,
		refreshRecentHistory,
		clearError: () => publish({ error: null }),
		startTwitchLogin,
		run: (workflow_id: string) => request(() => client.runWorkflow({ workflow_id })),
		approveTwitchLogin: (data: Parameters<DesktopClient['approveTwitchLogin']>[0]) =>
			request(() => client.approveTwitchLogin(data)),
		cancelTwitchLogin: (data: Parameters<DesktopClient['cancelTwitchLogin']>[0]) =>
			request(() => client.cancelTwitchLogin(data)),
		openTwitchLogin: (data: Parameters<DesktopClient['openTwitchLogin']>[0]) =>
			request(() => client.openTwitchLogin(data)),
		copyTwitchLogin: (data: Parameters<DesktopClient['copyTwitchLogin']>[0]) =>
			request(() => client.copyTwitchLogin(data)),
		submitInput: (data: Parameters<DesktopClient['submitInput']>[0]) =>
			request(() => client.submitInput(data)),
		cancelInput: (data: Parameters<DesktopClient['cancelInput']>[0]) =>
			request(() => client.cancelInput(data)),
		dismissError: (data: Parameters<DesktopClient['dismissError']>[0]) =>
			request(() => client.dismissError(data))
	};
}
