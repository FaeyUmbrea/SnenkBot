import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { expect, it, vi } from 'vitest';
import App from '../src/App.svelte';
import { createApplicationController, type DesktopClient } from '../src/lib/application';
import { createDesktopClient, DesktopRequestError } from '../src/lib/desktop';
import type {
	ConfigurationSnapshot,
	DesktopSnapshot,
	EditorSnapshot_Serialize
} from '../src/lib/contracts/index';
import { schemas, workflow } from './fixture';
import { twitchAccounts } from './account-fixture';

vi.mock('@tauri-apps/api/event', () => ({ emit: vi.fn(async () => undefined) }));

it('mounts the real application and preserves a settings draft while opening, editing and saving an automation', async () => {
	const snapshot: DesktopSnapshot = {
		sequence: 0,
		startup: { state: 'ready' },
		workflows: [
			{
				id: workflow.id,
				title: 'Stream details',
				enabled: true,
				revision: 1,
				step_count: 1,
				has_steps: true,
				error: null,
				category: 'Broadcast',
				trigger_summary: 'Manual',
				capabilities: [],
				capability_titles: {},
				integration_usage: {}
			}
		],
		connections: [],
		reconfiguration_requests: [],
		errors: [],
		last_completed: null,
		pending_input: null,
		twitch_authentication: null
	};
	let editor: EditorSnapshot_Serialize = {
		session_id: 'editor',
		trigger_schemas: {},
		revision: 0,
		draft: {
			name: 'Stream details',
			enabled: true,
			workflow: { ...workflow, steps: workflow.steps.slice(0, 1) },
			triggers: []
		},
		dirty: false,
		can_undo: false,
		can_redo: false,
		saved_revision: 1
	};
	const configuration: ConfigurationSnapshot = {
		session_id: 'obs',
		revision: 0,
		module: 'obs',
		schema: { ...schemas[0], title: 'OBS connection' },
		defaults: { title: '' },
		values: { title: 'Original' },
		authorization: null,
		actions: [],
		authorization_ready: false,
		collection: null,
		credential: null
	};
	const client: DesktopClient = {
		...createDesktopClient(async () => {
			throw new Error('Unexpected native request');
		}),
		actionDefinitions: vi.fn(async () =>
			schemas.map((schema) => ({ schema, defaults: { title: 'Native default' } }))
		),
		twitchAccounts: vi.fn(async () => twitchAccounts()),
		twitchLoginSnapshot: vi.fn(async () => null),
		historyPage: vi.fn(async () => ({ entries: [], next_cursor: null })),
		configurationCatalog: async () => [
			{
				id: 'obs',
				title: 'OBS Studio',
				category: 'broadcast',
				module: 'obs',
				record_id: null,
				error: null,
				configured: false
			},
			{
				id: 'api',
				title: 'Overlay API',
				category: 'general',
				module: 'api',
				record_id: null,
				error: null,
				configured: false
			}
		],
		openEditor: vi.fn(async () => editor),
		openConfiguration: vi.fn(async ({ module }) =>
			module === 'api'
				? {
						...configuration,
						session_id: 'api',
						module: 'api' as const,
						schema: { ...configuration.schema, title: 'API server' }
					}
				: configuration
		),
		configurationCredentialStatus: vi.fn(async () => 'empty' as const),
		closeEditor: vi.fn(async () => undefined),
		closeConfiguration: vi.fn(async () => undefined),
		applyEditorEdit: vi.fn(async (request) => {
			const edit = request.edit;
			if (edit.operation === 'rename')
				editor = { ...editor, draft: { ...editor.draft, name: edit.name } };
			editor = { ...editor, revision: editor.revision + 1, dirty: true, can_undo: true };
			return editor;
		}),
		saveEditor: vi.fn(async () => ({
			snapshot: { ...editor, dirty: false, saved_revision: 2 },
			notices: []
		}))
	};
	const controller = createApplicationController(client, (callbacks) => {
		callbacks.onSnapshot(snapshot);
		return { ready: Promise.resolve(), dispose: vi.fn(), refresh: vi.fn(async () => undefined) };
	});
	render(App, { client, controller });
	await screen.findByRole('heading', { name: 'Recent runs' });
	await fireEvent.click(screen.getByRole('button', { name: 'Broadcast Apps' }));
	await fireEvent.click(await screen.findByRole('button', { name: /OBS Studio/ }));
	const title = await screen.findByRole('textbox', { name: 'Title' });
	await fireEvent.input(title, { target: { value: 'Unsaved connection draft' } });
	await fireEvent.click(screen.getByRole('button', { name: 'Automations' }));
	await fireEvent.click(screen.getByRole('button', { name: 'Stream details' }));
	const name = await screen.findByRole('textbox', { name: 'Automation name' });
	await fireEvent.change(name, { target: { value: 'Updated details' } });
	await waitFor(() =>
		expect(client.applyEditorEdit).toHaveBeenCalledWith({
			session_id: 'editor',
			expected_revision: 0,
			edit: { operation: 'rename', name: 'Updated details' }
		})
	);
	await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
	await waitFor(() => expect(client.saveEditor).toHaveBeenCalled());
	await fireEvent.click(screen.getByRole('button', { name: 'Broadcast Apps' }));
	await fireEvent.click(await screen.findByRole('button', { name: /OBS Studio/ }));
	expect((screen.getByRole('textbox', { name: 'Title' }) as HTMLInputElement).value).toBe(
		'Unsaved connection draft'
	);
	expect(client.openConfiguration).toHaveBeenCalledTimes(1);
	vi.mocked(client.openConfiguration).mockRejectedValueOnce(
		new DesktopRequestError({
			code: 'unavailable',
			message: 'Settings are temporarily unavailable.'
		})
	);
	await fireEvent.click(screen.getByRole('button', { name: 'General' }));
	await screen.findByRole('heading', { name: 'Settings could not be opened' });
	expect(screen.getByRole('button', { name: 'Retry settings' })).toBeTruthy();
	await fireEvent.click(screen.getByRole('button', { name: 'Retry settings' }));

	await screen.findByRole('region', { name: 'API server settings' });
	expect(screen.getByRole('textbox', { name: 'Title' })).toBeTruthy();
	expect(client.openConfiguration).toHaveBeenLastCalledWith({ module: 'api', record_id: null });
	expect(screen.queryByRole('button', { name: /^Add$/ })).toBeNull();
	vi.mocked(client.openConfiguration).mockResolvedValueOnce({
		...configuration,
		module: 'pronouns',
		session_id: 'pronouns',
		schema: { ...configuration.schema, title: 'Pronouns' }
	});
	await controller.openSettings('pronouns');
	await screen.findByRole('region', { name: 'Pronouns settings' });
	expect(screen.getByRole('textbox', { name: 'Title' })).toBeTruthy();
	await fireEvent.click(screen.getByRole('button', { name: 'Back to previous view' }));
	await screen.findByRole('region', { name: 'API server settings' });
	expect(screen.queryByRole('region', { name: 'Pronouns settings' })).toBeNull();
});

it('opens the account recovery view from its catalog entry after the first account read fails', async () => {
	const snapshot: DesktopSnapshot = {
		sequence: 0,
		startup: { state: 'ready' },
		workflows: [],
		connections: [],
		reconfiguration_requests: [],
		errors: [],
		last_completed: null,
		pending_input: null,
		twitch_authentication: null
	};
	const accounts = vi
		.fn<DesktopClient['twitchAccounts']>()
		.mockRejectedValueOnce(
			new DesktopRequestError({ code: 'unavailable', message: 'Accounts temporarily unavailable.' })
		)
		.mockResolvedValue(twitchAccounts());
	const client: DesktopClient = {
		...createDesktopClient(async () => {
			throw new Error('Unexpected native request');
		}),
		actionDefinitions: vi.fn(async () => []),
		twitchAccounts: accounts,
		twitchLoginSnapshot: vi.fn(async () => null),
		historyPage: vi.fn(async () => ({ entries: [], next_cursor: null })),
		configurationCatalog: vi.fn(async () => [
			{
				id: 'twitch',
				title: 'Twitch',
				category: 'streaming' as const,
				module: null,
				record_id: null,
				configured: false,
				error: null
			}
		])
	};
	const controller = createApplicationController(client, (callbacks) => {
		callbacks.onSnapshot(snapshot);
		return { ready: Promise.resolve(), dispose: vi.fn(), refresh: vi.fn(async () => undefined) };
	});
	render(App, { client, controller });
	await waitFor(() => expect(accounts).toHaveBeenCalledTimes(1));
	await fireEvent.click(screen.getByRole('button', { name: 'Streaming Services' }));
	await fireEvent.click(await screen.findByRole('button', { name: /Twitch/ }));
	expect(screen.getByText('Accounts temporarily unavailable.')).toBeTruthy();
	await fireEvent.click(screen.getByRole('button', { name: 'Retry accounts' }));
	await screen.findByRole('region', { name: 'Twitch accounts' });
	expect(accounts).toHaveBeenCalledTimes(2);
	expect(screen.getByText('No account connected')).toBeTruthy();
});
