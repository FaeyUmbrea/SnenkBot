import { cleanup, fireEvent, render, waitFor, within } from '@testing-library/svelte';
import { get } from 'svelte/store';
import { afterEach, expect, it, vi } from 'vitest';
import EditorView from '../src/lib/EditorView.svelte';
import { createApplicationController, type DesktopClient } from '../src/lib/application';
import { createDesktopClient } from '../src/lib/desktop';
import type {
	ConfigSchema,
	DesktopSnapshot,
	EditorSnapshot_Serialize
} from '../src/lib/contracts/index';
import { twitchAccounts } from './account-fixture';

afterEach(cleanup);

it('keeps resource selections visible across draft revisions and saves collections separately from single choices', async () => {
	const schema: ConfigSchema = {
		id: 'test.deliver',
		version: 1,
		title: 'Deliver event',
		outputs: [],
		fields: [
			{
				id: 'recipients',
				label: 'Recipients',
				description: '',
				introduced_in: 1,
				kind: 'string_set',
				required: true,
				choice_source: { key: 'test.recipients', depends_on: null }
			},
			{
				id: 'mode',
				label: 'Mode',
				description: '',
				introduced_in: 1,
				kind: 'text',
				required: true,
				choice_source: { key: 'test.modes', depends_on: null }
			}
		]
	};
	let editor: EditorSnapshot_Serialize = {
		session_id: 'test-editor',
		revision: 0,
		draft: {
			name: 'Test delivery',
			enabled: true,
			triggers: [],
			workflow: {
				id: 'test',
				revision: 1,
				overlap: false,
				outputs: {},
				steps: [
					{
						id: 'deliver',
						on_failure: 'Stop',
						kind: {
							Action: {
								capability: schema.id,
								version: 1,
								deadline_ms: null,
								inputs: { recipients: { Literal: [] }, mode: { Literal: 'once' } }
							}
						}
					}
				]
			}
		},
		dirty: false,
		can_undo: false,
		can_redo: false,
		saved_revision: 1
	};
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
	const client: DesktopClient = {
		...createDesktopClient(async () => {
			throw new Error('Unexpected request');
		}),
		actionDefinitions: vi.fn(async () => [{ schema, defaults: { recipients: [], mode: 'once' } }]),
		configurationCatalog: vi.fn(async () => []),
		twitchAccounts: vi.fn(async () => twitchAccounts()),
		twitchLoginSnapshot: vi.fn(async () => null),
		historyPage: vi.fn(async () => ({ entries: [], next_cursor: null })),
		openEditor: vi.fn(async () => editor),
		closeEditor: vi.fn(async () => undefined),
		actionChoices: vi.fn(async ({ field_id }) =>
			field_id === 'recipients'
				? [
						{ value: 'uuid-one', label: 'Main overlay', detail: null },
						{ value: 'uuid-two', label: 'Chat overlay', detail: null }
					]
				: [
						{ value: 'once', label: 'Once', detail: null },
						{ value: 'repeat', label: 'Repeat', detail: null }
					]
		),
		applyEditorEdit: vi.fn(async ({ edit }) => {
			if (edit.operation !== 'set_input') throw new Error('Unexpected edit');
			const steps = structuredClone(editor.draft.workflow.steps);
			const action = steps[0].kind;
			if (typeof action === 'string' || !action.Action) throw new Error('Missing action');
			action.Action.inputs[edit.field_id] = edit.value;
			editor = {
				...editor,
				revision: editor.revision + 1,
				dirty: true,
				draft: { ...editor.draft, workflow: { ...editor.draft.workflow, steps } }
			};
			return editor;
		}),
		saveEditor: vi.fn(async () => ({ snapshot: { ...editor, dirty: false }, notices: [] }))
	};
	const controller = createApplicationController(client, (callbacks) => {
		callbacks.onSnapshot(snapshot);
		return { ready: Promise.resolve(), dispose: vi.fn(), refresh: vi.fn(async () => undefined) };
	});
	await controller.initialize();
	await controller.openWorkflow('test');
	controller.selectStep('deliver');
	await waitFor(() => expect(get(controller).schemas).toHaveLength(1));
	const view = render(EditorView, { controller });
	await fireEvent.click(view.getByRole('button', { name: 'Details' }));
	await fireEvent.click(view.getByRole('button', { name: 'Choose recipients' }));
	const group = await view.findByRole('group', { name: 'Recipients choices' });
	await fireEvent.click(await within(group).findByRole('button', { name: 'Main overlay' }));
	await waitFor(() =>
		expect(
			within(group).getByRole('button', { name: 'Main overlay' }).getAttribute('aria-pressed')
		).toBe('true')
	);
	await waitFor(() =>
		expect(within(group).getByRole('button', { name: 'Chat overlay' })).toHaveProperty(
			'disabled',
			false
		)
	);
	await fireEvent.click(within(group).getByRole('button', { name: 'Chat overlay' }));
	await waitFor(() =>
		expect(view.getByRole('button', { name: 'Choose recipients' }).textContent).toBe(
			'Main overlay, Chat overlay'
		)
	);
	await waitFor(() =>
		expect(within(group).getByRole('button', { name: 'Main overlay' })).toHaveProperty(
			'disabled',
			false
		)
	);
	await fireEvent.click(within(group).getByRole('button', { name: 'Main overlay' }));
	await waitFor(() =>
		expect(view.getByRole('button', { name: 'Choose recipients' }).textContent).toBe('Chat overlay')
	);
	expect(view.container.textContent).not.toContain('uuid-');
	await fireEvent.click(view.getByRole('button', { name: 'Choose mode' }));
	const modes = await view.findByRole('group', { name: 'Mode choices' });
	await fireEvent.click(await within(modes).findByRole('button', { name: 'Repeat' }));
	await waitFor(() =>
		expect(view.getByRole('button', { name: 'Choose mode' }).textContent).toBe('Repeat')
	);
	await fireEvent.click(view.getByRole('button', { name: 'Save' }));
	await waitFor(() => expect(client.saveEditor).toHaveBeenCalled());
	expect(editor.draft.workflow.steps[0].kind).toMatchObject({
		Action: { inputs: { recipients: { Literal: ['uuid-two'] }, mode: { Literal: 'repeat' } } }
	});
	await controller.dispose();
});
