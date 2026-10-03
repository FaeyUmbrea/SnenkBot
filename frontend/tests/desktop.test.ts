import { describe, expect, it, vi } from 'vitest';
import {
	createDesktopClient,
	DesktopRequestError,
	type DesktopTransport
} from '../src/lib/desktop';

describe('desktop editor boundary', () => {
	it('sends trigger insertion, arrangement and removal as single revision-guarded edits', async () => {
		const transport = vi.fn<DesktopTransport>().mockResolvedValue(undefined);
		const client = createDesktopClient(transport);
		const edit = {
			operation: 'insert_trigger_at' as const,
			kind: { kind: 'obs.recording_started' as const },
			position: { Before: { trigger_id: 'anchor' } }
		};
		await client.applyEditorEdit({ session_id: 'session', expected_revision: 1, edit });
		expect(transport).toHaveBeenCalledExactlyOnceWith('editor_apply', {
			request: { session_id: 'session', expected_revision: 1, edit }
		});
		await client.applyEditorEdit({
			session_id: 'session',
			expected_revision: 2,
			edit: { operation: 'move_trigger_to', trigger_id: 'recording', position: 'Append' }
		});
		await client.applyEditorEdit({
			session_id: 'session',
			expected_revision: 3,
			edit: { operation: 'remove_trigger', trigger_id: 'recording' }
		});
		expect(transport).toHaveBeenCalledTimes(3);
	});
	it('receives module defaults without inventing values for omitted optional fields', async () => {
		const definitions = [
			{
				schema: {
					id: 'twitch.send_chat',
					version: 1,
					title: 'Send chat message',
					fields: [],
					outputs: []
				},
				defaults: { message: '' }
			}
		];
		const transport = vi.fn<DesktopTransport>().mockResolvedValue(definitions);
		const client = createDesktopClient(transport);
		const result = await client.actionDefinitions();
		expect(transport).toHaveBeenCalledExactlyOnceWith('action_definitions', undefined);
		expect(result).toEqual(definitions);
		expect(result[0].defaults).not.toHaveProperty('broadcaster_fallback');
	});

	it('discovers resources explicitly using stable schema and dependency identities', async () => {
		const choices = [{ value: 'scene:stable-id', label: 'Main Scene', detail: null }];
		const transport = vi.fn<DesktopTransport>().mockResolvedValue(choices);
		const client = createDesktopClient(transport);
		expect(transport).not.toHaveBeenCalled();
		const data = {
			action_id: 'obs.set_item_enabled',
			field_id: 'item',
			depends_on: 'scene:stable-id'
		};
		await expect(client.actionChoices(data)).resolves.toEqual(choices);
		expect(transport).toHaveBeenCalledExactlyOnceWith('action_choices', { request: data });
		transport.mockRejectedValue({
			code: 'unavailable',
			message: 'Resources could not be loaded. Check the connection and try again.'
		});
		await expect(client.actionChoices(data)).rejects.toMatchObject({
			detail: { code: 'unavailable' }
		});
		transport.mockRejectedValue(new Error('private provider diagnostics'));
		await expect(client.actionChoices(data)).rejects.toThrow(
			'The application could not complete this request.'
		);
	});
	it('opens only an explicit current attempt and preserves stale authentication recovery', async () => {
		const transport = vi.fn<DesktopTransport>().mockResolvedValue(undefined);
		const client = createDesktopClient(transport);
		await client.startTwitchLogin({ role: 'bot' });
		expect(transport).toHaveBeenCalledTimes(1);
		expect(transport).toHaveBeenCalledWith('twitch_login_start', { request: { role: 'bot' } });
		await client.openTwitchLogin({ attempt_id: 'current' });
		expect(transport).toHaveBeenLastCalledWith('twitch_login_open_browser', {
			request: { attempt_id: 'current' }
		});
		transport.mockRejectedValue({
			code: 'stale_attempt',
			message: 'This sign-in has changed. Use the current sign-in.'
		});
		await expect(
			client.approveTwitchLogin({ attempt_id: 'old', user_id: '123' })
		).rejects.toMatchObject({ name: 'DesktopRequestError', detail: { code: 'stale_attempt' } });
		transport.mockRejectedValue('private token data');
		await expect(client.cancelTwitchLogin({ attempt_id: 'current' })).rejects.toThrow(
			'The application could not complete this request.'
		);
	});

	it('keeps dialog identity and values intact and presents recoverable input failures', async () => {
		const transport = vi.fn<DesktopTransport>().mockResolvedValue(undefined);
		const client = createDesktopClient(transport);
		await client.submitInput({ request_id: 'current-dialog', values: { title: 'New title' } });
		expect(transport).toHaveBeenCalledWith('submit_input', {
			request: { request_id: 'current-dialog', values: { title: 'New title' } }
		});
		transport.mockRejectedValue({ kind: 'stale_request' });
		await expect(client.cancelInput({ request_id: 'old-dialog' })).rejects.toMatchObject({
			name: 'DesktopInputError',
			kind: 'stale_request',
			message: 'This dialog has changed. Use the current dialog.'
		});
		transport.mockRejectedValue({ kind: 'emission_failed', message: '/private/internal' });
		await expect(client.submitInput({ request_id: 'current-dialog', values: {} })).rejects.toThrow(
			'The input dialog could not be updated.'
		);
	});
	it('sends the session revision and stable placement identity to Rust unchanged', async () => {
		const transport = vi.fn<DesktopTransport>().mockResolvedValue(undefined);
		const client = createDesktopClient(transport);
		await client.applyEditorEdit({
			session_id: 'editor-one',
			expected_revision: 17,
			edit: {
				operation: 'move_to',
				step_id: 'moving-step',
				destination: { Branch: { parent_id: 'condition', branch: 'Else' } },
				position: { Before: { step_id: 'next-step' } }
			}
		});
		expect(transport).toHaveBeenCalledWith('editor_apply', {
			request: {
				session_id: 'editor-one',
				expected_revision: 17,
				edit: {
					operation: 'move_to',
					step_id: 'moving-step',
					destination: { Branch: { parent_id: 'condition', branch: 'Else' } },
					position: { Before: { step_id: 'next-step' } }
				}
			}
		});
	});

	it('preserves typed recovery errors while hiding unstructured transport details', async () => {
		const transport = vi.fn<DesktopTransport>().mockRejectedValue({
			code: 'save_conflict',
			message: 'This workflow changed on disk. Your draft is still open.'
		});
		const client = createDesktopClient(transport);
		await expect(
			client.saveEditor({ session_id: 'editor-one', expected_revision: 17 })
		).rejects.toMatchObject({
			name: 'DesktopRequestError',
			detail: { code: 'save_conflict' }
		});
		transport.mockRejectedValue('internal worker path /private/data');
		await expect(client.openEditor({ workflow_id: 'workflow' })).rejects.toEqual(
			new DesktopRequestError('internal worker path /private/data')
		);
		await expect(client.openEditor({ workflow_id: 'workflow' })).rejects.toThrow(
			'The application could not complete this request.'
		);
	});
});
