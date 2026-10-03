import { describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import { createApplicationController, type DesktopClient } from '../src/lib/application';
import { createDesktopClient } from '../src/lib/desktop';
import { subscribeDesktopUpdates, type DesktopFeedCallbacks } from '../src/lib/desktop-feed';
import type {
	ConfigurationSnapshot,
	ConnectionModule,
	DesktopSnapshot,
	EditorSnapshot_Serialize,
	HistoryPage,
	TwitchAccountsSnapshot,
	TwitchAuthentication
} from '../src/lib/contracts/index';
import { schemas, workflow } from './fixture';
const definitions = schemas.map((schema) => ({ schema, defaults: { native: true } }));

function deferred<T>() {
	let resolve!: (value: T) => void;
	let reject!: (error: unknown) => void;
	const promise = new Promise<T>((accept, fail) => {
		resolve = accept;
		reject = fail;
	});
	return { promise, resolve, reject };
}
function snapshot(sequence = 0): DesktopSnapshot {
	return {
		sequence,
		startup: { state: 'ready' },
		workflows: [],
		connections: [],
		reconfiguration_requests: [],
		errors: [],
		last_completed: null,
		pending_input: null,
		twitch_authentication: null
	};
}
function editor(id = 'one', revision = 0): EditorSnapshot_Serialize {
	return {
		session_id: `session-${id}`,
		revision,
		draft: { enabled: true, workflow: { ...workflow, id }, triggers: [] },
		dirty: revision > 0,
		can_undo: revision > 0,
		can_redo: false,
		saved_revision: 0
	};
}
function configuration(module: ConnectionModule = 'obs'): ConfigurationSnapshot {
	return {
		session_id: `config-${module}`,
		revision: 0,
		module,
		schema: schemas[0],
		defaults: {},
		values: { title: 'Original' }
	};
}
function harness(overrides: Partial<DesktopClient> = {}, initial = snapshot()) {
	let callbacks!: DesktopFeedCallbacks;
	const dispose = vi.fn();
	const factory = vi.fn((value: DesktopFeedCallbacks) => {
		callbacks = value;
		value.onSnapshot(initial);
		return { ready: Promise.resolve(), refresh: vi.fn(async () => undefined), dispose };
	});
	const client: DesktopClient = {
		...createDesktopClient(async () => {
			throw new Error('Unexpected fixture request');
		}),
		actionDefinitions: vi.fn(async () => definitions),
		twitchAccounts: vi.fn(async () => ({ broadcaster: null, bot: null })),
		twitchLoginSnapshot: vi.fn(async () => null),
		historyPage: vi.fn(async () => ({ entries: [], next_cursor: null })),
		openEditor: vi.fn(async ({ workflow_id }) => editor(workflow_id)),
		openConfiguration: vi.fn(async ({ module }) => configuration(module)),
		closeEditor: vi.fn(async () => undefined),
		closeConfiguration: vi.fn(async () => undefined),
		...overrides
	};
	const controller = createApplicationController(client, factory);
	return {
		client,
		controller,
		factory,
		dispose,
		send: (value: DesktopSnapshot) => callbacks.onSnapshot(value)
	};
}

describe('application lifetime', () => {
	it('owns resource discovery for the selected draft and preserves it through a settings detour', async () => {
		const valueSources = vi.fn<DesktopClient['valueSources']>().mockResolvedValue([]);
		const h = harness({ valueSources, applyEditorEdit: vi.fn(async () => editor('one', 1)) });
		await h.controller.initialize();
		expect(get(h.controller).definitions).toEqual(definitions);
		expect(get(h.controller).schemas).toEqual(schemas);
		await h.controller.openWorkflow('one');
		h.controller.selectStep('title');
		await h.controller.resources.loadValues();
		expect(valueSources).toHaveBeenCalledExactlyOnceWith({
			session_id: 'session-one',
			expected_revision: 0,
			step_id: 'title'
		});
		const context = get(h.controller.resources).context;
		await h.controller.openSettings('obs');
		expect(get(h.controller.resources).context).toEqual(context);
		h.controller.returnToContext();
		await h.controller.edit({ operation: 'rename', name: 'Changed' });
		expect(get(h.controller.resources).context?.expected_revision).toBe(1);
		expect(get(h.controller.resources).values).toEqual([]);
		h.controller.selectStep(null);
		expect(get(h.controller.resources).context).toBeNull();
		await h.controller.dispose();
		await h.controller.resources.loadValues();
		expect(valueSources).toHaveBeenCalledTimes(1);
	});

	it('reads bounded recent history at readiness and coalesces new completion bursts without polling', async () => {
		const first = deferred<HistoryPage>();
		const historyPage = vi
			.fn<DesktopClient['historyPage']>()
			.mockReturnValueOnce(first.promise)
			.mockResolvedValue({ entries: [], next_cursor: 'more' });
		const h = harness({ historyPage });
		const initializing = h.controller.initialize();
		const completed = (id: string, sequence: number) => ({
			...snapshot(sequence),
			last_completed: {
				run_id: id,
				workflow_id: 'one',
				revision: 1,
				outcome: 'Success' as const
			}
		});
		h.send(completed('run-one', 1));
		h.send(completed('run-one', 2));
		h.send(completed('run-two', 3));
		expect(historyPage).toHaveBeenCalledTimes(1);
		expect(h.controller.refreshRecentHistory()).toBe(h.controller.refreshRecentHistory());
		first.resolve({ entries: [], next_cursor: null });
		await initializing;
		expect(historyPage).toHaveBeenCalledTimes(2);
		expect(historyPage).toHaveBeenLastCalledWith({
			page_size: 6,
			cursor: null,
			filter: { workflow_id: null, outcome: null, trigger_kind: null }
		});
		expect(get(h.controller).recentHistory?.next_cursor).toBe('more');
		h.send(completed('run-two', 4));
		expect(historyPage).toHaveBeenCalledTimes(2);
		await h.controller.dispose();
	});
	it('preserves cached history after failures, retries explicitly and ignores reads after disposal', async () => {
		const late = deferred<HistoryPage>();
		const historyPage = vi
			.fn<DesktopClient['historyPage']>()
			.mockResolvedValueOnce({ entries: [], next_cursor: 'cached' })
			.mockRejectedValueOnce(new Error('private path'))
			.mockReturnValueOnce(late.promise);
		const h = harness({ historyPage });
		await h.controller.initialize();
		await h.controller.refreshRecentHistory();
		expect(get(h.controller).recentHistory?.next_cursor).toBe('cached');
		expect(get(h.controller).recentHistoryError).toBe(
			'The application could not complete this request.'
		);
		expect(get(h.controller).error).toBeNull();
		const refreshing = h.controller.refreshRecentHistory();
		await h.controller.dispose();
		const before = get(h.controller);
		late.resolve({ entries: [], next_cursor: 'late' });
		await refreshing;
		expect(get(h.controller)).toBe(before);
		await h.controller.refreshRecentHistory();
		expect(historyPage).toHaveBeenCalledTimes(3);
	});

	it('explicit retry replaces a failed native subscription without duplicating a healthy listener', async () => {
		const h = harness();
		const stop = vi.fn();
		const listen = vi
			.fn()
			.mockRejectedValueOnce(new Error('native failure'))
			.mockResolvedValue(stop);
		const factory = vi.fn((callbacks: DesktopFeedCallbacks) =>
			subscribeDesktopUpdates(callbacks, { listen, snapshot: async () => snapshot() })
		);
		const controller = createApplicationController(h.client, factory);
		await controller.initialize();
		expect(get(controller).snapshot).toBeNull();
		await controller.retryStartup();
		expect(get(controller).snapshot?.startup.state).toBe('ready');
		expect(get(controller).schemas).toEqual(schemas);
		await controller.retryStartup();
		expect(factory).toHaveBeenCalledTimes(2);
		expect(listen).toHaveBeenCalledTimes(2);
		expect(stop).not.toHaveBeenCalled();
		await controller.dispose();
		expect(stop).toHaveBeenCalledTimes(1);
	});
	it('owns one subscription and bootstraps once after Ready even with synchronous callbacks', async () => {
		const initial = snapshot();
		initial.startup = { state: 'starting' };
		const h = harness({}, initial);
		await h.controller.initialize();
		await h.controller.initialize();
		expect(h.client.actionDefinitions).not.toHaveBeenCalled();
		h.send(snapshot(1));
		h.send(snapshot(2));
		await vi.waitFor(() => expect(get(h.controller).schemas).toEqual(schemas));
		expect(h.factory).toHaveBeenCalledTimes(1);
		expect(h.client.actionDefinitions).toHaveBeenCalledTimes(1);
		expect(h.client.twitchAccounts).toHaveBeenCalledTimes(1);
		await h.controller.dispose();
		await h.controller.dispose();
		expect(h.dispose).toHaveBeenCalledTimes(1);
	});
	it('preserves sessions, selection and view state through nested settings detours', async () => {
		const h = harness();
		await h.controller.initialize();
		await h.controller.openWorkflow('one');
		h.controller.selectStep('title');
		h.controller.setSearch('scene');
		h.controller.setScroll(240);
		await h.controller.openSettings('obs');
		h.controller.setSearch('host');
		h.controller.setScroll(50);
		await h.controller.openSettings('vtube_studio');
		h.controller.returnToContext();
		expect(get(h.controller).configurationModule).toBe('obs');
		h.controller.returnToContext();
		expect(get(h.controller).selectedStepId).toBe('title');
		expect(get(h.controller).views['workflow:one']).toEqual({ search: 'scene', scroll: 240 });
		await h.controller.openSettings('obs');
		expect(get(h.controller).views['configuration:obs']).toEqual({ search: 'host', scroll: 50 });
		expect(h.client.openConfiguration).toHaveBeenCalledTimes(2);
		expect(get(h.controller).configurations.obs?.session_id).toBe('config-obs');
		expect(get(h.controller).configurations.vtube_studio?.session_id).toBe('config-vtube_studio');
		expect(h.client.openEditor).toHaveBeenCalledTimes(1);
		await h.controller.dispose();
		expect(h.client.closeConfiguration).toHaveBeenCalledTimes(2);
		expect(h.client.closeConfiguration).toHaveBeenCalledWith({ session_id: 'config-obs' });
		expect(h.client.closeConfiguration).toHaveBeenCalledWith({ session_id: 'config-vtube_studio' });
	});
	it('late workflow opens populate only their cache and never change the selected workflow', async () => {
		const opening = deferred<EditorSnapshot_Serialize>();
		const h = harness({
			openEditor: vi.fn(({ workflow_id }) =>
				workflow_id === 'one' ? opening.promise : Promise.resolve(editor(workflow_id))
			)
		});
		const first = h.controller.openWorkflow('one');
		await h.controller.openWorkflow('two');
		opening.resolve(editor('one'));
		await first;
		expect(get(h.controller).workflowId).toBe('two');
		expect(get(h.controller).editor?.session_id).toBe('session-two');
		expect(get(h.controller).editors.one).toEqual(editor('one'));
		await h.controller.dispose();
	});
	it('serializes edits and saves using the preceding revision and survives inventory updates', async () => {
		const editing = deferred<EditorSnapshot_Serialize>();
		const applyEditorEdit = vi
			.fn<DesktopClient['applyEditorEdit']>()
			.mockReturnValueOnce(editing.promise)
			.mockResolvedValueOnce(editor('one', 2));
		const saveEditor = vi
			.fn<DesktopClient['saveEditor']>()
			.mockResolvedValue({ snapshot: { ...editor('one', 2), dirty: false }, notices: [] });
		const h = harness({ applyEditorEdit, saveEditor });
		await h.controller.initialize();
		await h.controller.openWorkflow('one');
		const first = h.controller.edit({ operation: 'rename', name: 'First' });
		const second = h.controller.edit({ operation: 'enabled', enabled: false });
		const saving = h.controller.saveEditor();
		await vi.waitFor(() => expect(applyEditorEdit).toHaveBeenCalledTimes(1));
		h.send(snapshot(5));
		await h.controller.openWorkflow('two');
		editing.resolve(editor('one', 1));
		await Promise.all([first, second, saving]);
		expect(applyEditorEdit.mock.calls.map(([request]) => request.expected_revision)).toEqual([
			0, 1
		]);
		expect(saveEditor).toHaveBeenCalledWith({ session_id: 'session-one', expected_revision: 2 });
		expect(get(h.controller).editor?.session_id).toBe('session-two');
		expect(get(h.controller).editors.one.dirty).toBe(false);
		await h.controller.dispose();
	});
	it('failed requests preserve working drafts and do not expose arbitrary native errors', async () => {
		const h = harness({
			saveEditor: vi.fn(async () => {
				throw new Error('secret native content');
			})
		});
		await h.controller.openWorkflow('one');
		const before = get(h.controller).editor;
		await expect(h.controller.saveEditor()).rejects.toThrow();
		expect(get(h.controller).editor).toEqual(before);
		expect(get(h.controller).error).toBe('The application could not complete this request.');
		expect(get(h.controller).loading).toBe(false);
		await h.controller.dispose();
	});
	it('disposal waits for pending opens, closes owned sessions once and prevents late publications', async () => {
		const opening = deferred<EditorSnapshot_Serialize>();
		const h = harness({ openEditor: vi.fn(() => opening.promise) });
		await h.controller.initialize();
		const first = h.controller.openWorkflow('one');
		const before = get(h.controller);
		const stopping = h.controller.dispose();
		h.send(snapshot(99));
		opening.resolve(editor());
		await first;
		await stopping;
		await h.controller.dispose();
		expect(get(h.controller)).toBe(before);
		expect(h.client.closeEditor).toHaveBeenCalledExactlyOnceWith({ session_id: 'session-one' });
		expect(h.dispose).toHaveBeenCalledTimes(1);
	});
	it('connected attempts refresh once, latest account request wins, and login replies cannot roll back feed auth', async () => {
		const login = deferred<TwitchAuthentication>();
		const old = deferred<TwitchAccountsSnapshot>();
		const latest = { broadcaster: { user_id: 'new', login: 'new' }, bot: null };
		const twitchAccounts = vi
			.fn<DesktopClient['twitchAccounts']>()
			.mockResolvedValueOnce({ broadcaster: null, bot: null })
			.mockReturnValueOnce(old.promise)
			.mockResolvedValueOnce(latest);
		const h = harness({ twitchAccounts, startTwitchLogin: vi.fn(() => login.promise) });
		await h.controller.initialize();
		const starting = h.controller.startTwitchLogin({ role: 'broadcaster' });
		const connected: TwitchAuthentication = {
			attempt_id: 'attempt',
			role: 'broadcaster',
			phase: { state: 'connected', data: { login: 'new' } }
		};
		h.send({ ...snapshot(1), twitch_authentication: connected });
		h.send({ ...snapshot(2), twitch_authentication: { ...connected } });
		expect(twitchAccounts).toHaveBeenCalledTimes(2);
		await h.controller.refreshAccounts();
		old.resolve({ broadcaster: { user_id: 'old', login: 'old' }, bot: null });
		login.resolve({ ...connected, phase: { state: 'starting' } });
		await starting;
		expect(get(h.controller).authentication?.phase.state).toBe('connected');
		expect(get(h.controller).accounts).toEqual(latest);
		await h.controller.dispose();
	});
	it('opens a specific history run and restores the originating workflow context', async () => {
		const h = harness();
		await h.controller.openWorkflow('one');
		h.controller.selectStep('title');
		h.controller.setScroll(125);
		h.controller.openHistory('selected-run');
		expect(get(h.controller).page).toBe('history');
		expect(get(h.controller).historyRunId).toBe('selected-run');
		h.controller.selectHistoryRun(null);
		await h.controller.openSettings('obs');
		h.controller.returnToContext();
		expect(get(h.controller).page).toBe('history');
		expect(get(h.controller).historyRunId).toBeNull();
		h.controller.returnToContext();
		expect(get(h.controller).selectedStepId).toBe('title');
		expect(get(h.controller).views['workflow:one'].scroll).toBe(125);
		await h.controller.dispose();
	});
	it('library navigation clears visible selection and keeps cached context for reopening', async () => {
		const h = harness();
		await h.controller.openWorkflow('one');
		h.controller.selectStep('title');
		h.controller.setSearch('remember');
		h.controller.showLibrary();
		h.controller.setSearch('library');
		expect(get(h.controller).workflowId).toBeNull();
		expect(get(h.controller).editor).toBeNull();
		await h.controller.openSettings('obs');
		h.controller.returnToContext();
		expect(get(h.controller).page).toBe('automations');
		expect(get(h.controller).workflowId).toBeNull();
		await h.controller.openWorkflow('one');
		expect(get(h.controller).selectedStepId).toBe('title');
		expect(get(h.controller).views['workflow:one'].search).toBe('remember');
		expect(get(h.controller).views.automations.search).toBe('library');
		expect(h.client.openEditor).toHaveBeenCalledTimes(1);
		await h.controller.dispose();
	});
	it('preserves recursive step selection when moved and clears it after removal', async () => {
		const moved = editor('one', 1);
		const removed = editor('one', 2);
		removed.draft.workflow = { ...removed.draft.workflow, steps: [] };
		const h = harness({
			applyEditorEdit: vi
				.fn<DesktopClient['applyEditorEdit']>()
				.mockResolvedValueOnce(moved)
				.mockResolvedValueOnce(removed)
		});
		await h.controller.openWorkflow('one');
		h.controller.selectStep('then-message');
		await h.controller.edit({
			operation: 'move_to',
			step_id: 'then-message',
			destination: 'Root',
			position: 'Append'
		});
		expect(get(h.controller).selectedStepId).toBe('then-message');
		await h.controller.edit({ operation: 'remove', step_id: 'then-message' });
		expect(get(h.controller).selectedStepId).toBeNull();
		await h.controller.dispose();
	});
	it('initialize waits for bootstrap already started by a synchronous feed callback', async () => {
		const catalog = deferred<typeof definitions>();
		const h = harness({ actionDefinitions: vi.fn(() => catalog.promise) });
		let ready = false;
		const initialization = h.controller.initialize().then(() => {
			ready = true;
		});
		await Promise.resolve();
		await Promise.resolve();
		expect(ready).toBe(false);
		catalog.resolve(definitions);
		await initialization;
		expect(get(h.controller).schemas).toEqual(schemas);
		await h.controller.dispose();
	});
	it('does not publish or repeat cleanup when disposed inside the first synchronous callback', async () => {
		const h = harness();
		let stopping: Promise<void> | undefined;
		const unsubscribe = h.controller.subscribe((state) => {
			if (state.snapshot && !stopping) stopping = h.controller.dispose();
		});
		await h.controller.initialize();
		await stopping;
		expect(h.dispose).toHaveBeenCalledTimes(1);
		expect(h.client.actionDefinitions).not.toHaveBeenCalled();
		h.send(snapshot(20));
		expect(get(h.controller).snapshot?.sequence).toBe(0);
		unsubscribe();
	});
	it('retries failed startup metadata explicitly and coalesces concurrent retries', async () => {
		const retry = deferred<typeof definitions>();
		const actionDefinitions = vi
			.fn<DesktopClient['actionDefinitions']>()
			.mockRejectedValueOnce(new Error('unavailable'))
			.mockReturnValueOnce(retry.promise);
		const h = harness({ actionDefinitions });
		await h.controller.initialize();
		expect(get(h.controller).schemas).toEqual([]);
		h.send(snapshot(1));
		h.send(snapshot(2));
		expect(actionDefinitions).toHaveBeenCalledTimes(1);
		const first = h.controller.retryStartup();
		const second = h.controller.retryStartup();
		expect(first).toBe(second);
		await vi.waitFor(() => expect(actionDefinitions).toHaveBeenCalledTimes(2));
		retry.resolve(definitions);
		await first;
		expect(get(h.controller).schemas).toEqual(schemas);
		expect(h.client.twitchLoginSnapshot).toHaveBeenCalledTimes(1);
		h.send(snapshot(3));
		expect(actionDefinitions).toHaveBeenCalledTimes(2);
		await h.controller.dispose();
	});
	it('opens a successfully created workflow and preserves errors on failure', async () => {
		const createWorkflow = vi
			.fn<DesktopClient['createWorkflow']>()
			.mockResolvedValueOnce('created')
			.mockRejectedValueOnce(new Error('failure'));
		const h = harness({ createWorkflow });
		await h.controller.createWorkflow('New automation');
		expect(createWorkflow).toHaveBeenCalledWith({ name: 'New automation' });
		expect(get(h.controller).workflowId).toBe('created');
		await expect(h.controller.createWorkflow('Other')).rejects.toThrow();
		expect(get(h.controller).workflowId).toBe('created');
		expect(h.client.openEditor).toHaveBeenCalledTimes(1);
		await h.controller.dispose();
	});
	it('late creation results preserve the route selected while creation was pending', async () => {
		const creation = deferred<string>();
		const h = harness({ createWorkflow: vi.fn(() => creation.promise) });
		const pending = h.controller.createWorkflow('New automation');
		h.controller.navigate('history');
		creation.resolve('created');
		await pending;
		expect(get(h.controller).page).toBe('history');
		expect(h.client.openEditor).not.toHaveBeenCalled();
		await h.controller.dispose();
	});
	it('readiness rejection is safely surfaced and does not start dependent requests', async () => {
		const client = createDesktopClient(
			vi.fn(async () => {
				throw new Error('Should not invoke');
			})
		);
		const controller = createApplicationController(client, () => ({
			ready: Promise.reject(new Error('native secret')),
			refresh: async () => undefined,
			dispose: vi.fn()
		}));
		await controller.initialize();
		expect(get(controller).error).toBe('The application could not complete this request.');
		expect(get(controller).snapshot).toBeNull();
		await controller.dispose();
	});
});
