import { DesktopRequestError } from '../src/lib/desktop';
import { fireEvent, render, waitFor } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import ConfigurationView from '../src/lib/ConfigurationView.svelte';
import type { ConfigurationSaveResult, ConfigurationSnapshot } from '../src/lib/contracts/index';

const snapshot: ConfigurationSnapshot = {
	session_id: 'session-a',
	revision: 3,
	module: 'obs',
	schema: {
		id: 'obs',
		version: 1,
		title: 'OBS connection',
		outputs: [],
		fields: [
			{
				id: 'host',
				label: 'Host',
				description: 'OBS WebSocket host.',
				introduced_in: 1,
				kind: 'text',
				required: true,
				choice_source: null
			},
			{
				id: 'port',
				label: 'Port',
				description: '',
				introduced_in: 1,
				kind: 'integer',
				required: true,
				choice_source: null
			},
			{
				id: 'enabled',
				label: 'Enabled',
				description: '',
				introduced_in: 1,
				kind: 'toggle',
				required: false,
				choice_source: null
			},
			{
				id: 'password',
				label: 'Password',
				description: '',
				introduced_in: 1,
				kind: 'secret',
				required: false,
				choice_source: null
			}
		]
	},
	defaults: { host: 'localhost', port: 4455, enabled: true, password: '' },
	values: { host: '', port: 4455, enabled: false, password: '' }
};

function saved(revision: number, notices: ConfigurationSaveResult['notices'] = []) {
	return {
		snapshot: {
			...snapshot,
			revision,
			values: { ...snapshot.values },
			defaults: { ...snapshot.defaults }
		},
		notices
	};
}

describe('ConfigurationView', () => {
	it('keeps failed drafts and hides arbitrary backend diagnostics from save errors', async () => {
		const onSave = vi.fn().mockRejectedValue(new Error('/private/config/backend'));
		const onSavePassword = vi.fn().mockRejectedValue(new Error('private token diagnostic'));
		const view = render(ConfigurationView, { snapshot, onSave, onSavePassword });
		await fireEvent.change(view.getByLabelText('Host'), { target: { value: 'localhost' } });
		await fireEvent.click(view.getByRole('button', { name: 'Save settings' }));
		await waitFor(() =>
			expect(view.getByRole('alert').textContent).toContain('Settings could not be saved')
		);
		expect(view.container.textContent).not.toContain('/private/config/backend');
		expect(view.getByLabelText('Host')).toHaveProperty('value', 'localhost');
		await fireEvent.click(view.getByRole('button', { name: 'Dismiss save error' }));
		await fireEvent.change(view.getByLabelText('OBS password'), {
			target: { value: 'fixture-only' }
		});
		await fireEvent.click(view.getByRole('button', { name: 'Save password' }));
		await waitFor(() =>
			expect(view.getByRole('alert').textContent).toContain('OBS password could not be saved')
		);
		expect(view.container.textContent).not.toContain('private token diagnostic');
		expect(view.getByLabelText('OBS password')).toHaveProperty('value', 'fixture-only');
	});

	it('uses schema values and defaults, and excludes credentials from portable saves', async () => {
		const onSave = vi.fn().mockResolvedValue(saved(4));
		const view = render(ConfigurationView, {
			snapshot,
			onSave,
			credentialPresence: 'stored',
			onSavePassword: vi.fn()
		});
		expect(view.getByLabelText('Host')).toHaveProperty('value', '');
		expect(view.getByLabelText('Port')).toHaveProperty('value', '4455');
		expect(view.getByLabelText('Enabled')).toHaveProperty('checked', false);
		expect(view.getByText('A password is stored. Enter a new one to replace it.')).toBeTruthy();
		expect(view.queryByLabelText('Password')).toBeNull();
		await fireEvent.change(view.getByLabelText('Host'), { target: { value: '127.0.0.1' } });
		await fireEvent.click(view.getByRole('button', { name: 'Save settings' }));
		await waitFor(() => expect(onSave).toHaveBeenCalledOnce());
		expect(onSave).toHaveBeenCalledWith({
			session_id: 'session-a',
			expected_revision: 3,
			values: { host: '127.0.0.1', port: 4455, enabled: false }
		});
	});

	it('surfaces storage durability notices after a successful save', async () => {
		const result = saved(4, ['directory_sync']);
		const onSave = vi.fn().mockResolvedValue(result);
		const view = render(ConfigurationView, { snapshot, onSave });
		await fireEvent.change(view.getByLabelText('Host'), { target: { value: 'localhost' } });
		await fireEvent.click(view.getByRole('button', { name: 'Save settings' }));
		await waitFor(() =>
			expect(view.getByRole('status').textContent).toContain('could not be synced')
		);
	});

	it('blocks saving after an external revision until the user reloads it', async () => {
		const onSave = vi.fn().mockResolvedValue(saved(6));
		const view = render(ConfigurationView, { snapshot, onSave });
		await fireEvent.change(view.getByLabelText('Host'), { target: { value: 'my edit' } });
		await view.rerender({
			snapshot: { ...snapshot, revision: 5, values: { ...snapshot.values, host: 'external' } },
			onSave
		});
		await waitFor(() =>
			expect(view.getByRole('alert').textContent).toContain('changed in another view')
		);
		expect(view.getByRole('button', { name: 'Save settings' })).toHaveProperty('disabled', true);
		expect(view.getByLabelText('Host')).toHaveProperty('value', 'my edit');
		await fireEvent.click(view.getByRole('button', { name: 'Reload current values' }));
		expect(view.getByLabelText('Host')).toHaveProperty('value', 'external');
		await fireEvent.change(view.getByLabelText('Host'), { target: { value: 'new edit' } });
		await fireEvent.click(view.getByRole('button', { name: 'Save settings' }));
		await waitFor(() => expect(onSave).toHaveBeenCalledOnce());
		expect(onSave).toHaveBeenCalledWith(expect.objectContaining({ expected_revision: 5 }));
	});

	it('keeps field edits made while settings are saving', async () => {
		let finish!: (value: ReturnType<typeof saved>) => void;
		const pending = new Promise<ReturnType<typeof saved>>((resolve) => {
			finish = resolve;
		});
		const onSave = vi.fn().mockReturnValue(pending);
		const onSavePassword = vi.fn();
		const view = render(ConfigurationView, { snapshot, onSave, onSavePassword });
		await fireEvent.change(view.getByLabelText('Host'), { target: { value: 'first edit' } });
		await fireEvent.click(view.getByRole('button', { name: 'Save settings' }));
		await fireEvent.change(view.getByLabelText('Host'), { target: { value: 'newer edit' } });
		const result = saved(4);
		result.snapshot.values.host = 'first edit';
		finish(result);
		await waitFor(() => expect(view.getByRole('status').textContent).toContain('saved'));
		expect(onSavePassword).not.toHaveBeenCalled();
		expect(view.getByLabelText('Host')).toHaveProperty('value', 'newer edit');
	});

	it('accepts the owning save snapshot and adopts its canonical values', async () => {
		let updateSnapshot: (value: ConfigurationSnapshot) => Promise<void> = async () => {};
		const result = saved(4);
		result.snapshot.values.host = 'canonical host';
		const onSave = vi.fn(async () => {
			await updateSnapshot(result.snapshot);
			return result;
		});
		const view = render(ConfigurationView, { snapshot, onSave });
		updateSnapshot = async (value) => {
			await view.rerender({ snapshot: value, onSave });
		};
		await fireEvent.change(view.getByLabelText('Host'), { target: { value: '  entered host  ' } });
		await fireEvent.click(view.getByRole('button', { name: 'Save settings' }));
		await waitFor(() =>
			expect(view.getByLabelText('Host')).toHaveProperty('value', 'canonical host')
		);
		expect(view.queryByText(/changed in another view/)).toBeNull();
	});

	it('ignores a late save result after switching to another session', async () => {
		let finish!: (value: ReturnType<typeof saved>) => void;
		const pending = new Promise<ReturnType<typeof saved>>((resolve) => {
			finish = resolve;
		});
		const view = render(ConfigurationView, { snapshot, onSave: vi.fn().mockReturnValue(pending) });
		await fireEvent.change(view.getByLabelText('Host'), { target: { value: 'old session draft' } });
		await fireEvent.click(view.getByRole('button', { name: 'Save settings' }));
		const next = {
			...snapshot,
			session_id: 'session-b',
			revision: 0,
			values: { ...snapshot.values, host: 'new session' }
		};
		await view.rerender({ snapshot: next, onSave: vi.fn() });
		await waitFor(() => expect(view.getByLabelText('Host')).toHaveProperty('value', 'new session'));
		const result = saved(4);
		result.snapshot.values.host = 'draft host';
		finish(result);
		await waitFor(() => expect(view.getByLabelText('Host')).toHaveProperty('value', 'new session'));
		expect(view.queryByRole('status')).toBeNull();
	});

	it('leaves an empty integer empty and lets the required number input block saving', async () => {
		const onSave = vi.fn().mockResolvedValue(saved(4));
		const view = render(ConfigurationView, { snapshot, onSave });
		await fireEvent.change(view.getByLabelText('Port'), { target: { value: '' } });
		expect(view.getByLabelText('Port')).toHaveProperty('value', '');
		expect(view.getByLabelText('Port')).toHaveProperty('step', '1');
		await fireEvent.click(view.getByRole('button', { name: 'Save settings' }));
		expect(onSave).not.toHaveBeenCalled();
	});

	it('retains failed drafts and prevents a second save while the first is pending', async () => {
		let finish!: (value: ReturnType<typeof saved>) => void;
		const pending = new Promise<ReturnType<typeof saved>>((resolve) => {
			finish = resolve;
		});
		const onSave = vi.fn().mockReturnValue(pending);
		const view = render(ConfigurationView, { snapshot, onSave });
		await fireEvent.change(view.getByLabelText('Host'), { target: { value: 'draft host' } });
		await fireEvent.click(view.getByRole('button', { name: 'Save settings' }));
		await fireEvent.click(view.getByRole('button', { name: 'Saving…' }));
		expect(onSave).toHaveBeenCalledOnce();
		const result = saved(4);
		result.snapshot.values.host = 'draft host';
		finish(result);
		await waitFor(() => expect(view.getByRole('status').textContent).toContain('saved'));
		expect(view.getByLabelText('Host')).toHaveProperty('value', 'draft host');
	});

	it('keeps an unsaved edit through a same-session external revision and resets for a new session', async () => {
		const view = render(ConfigurationView, {
			snapshot,
			onSave: vi.fn().mockResolvedValue(saved(4))
		});
		await fireEvent.change(view.getByLabelText('Host'), { target: { value: 'my edit' } });
		await view.rerender({
			snapshot: { ...snapshot, revision: 4, values: { ...snapshot.values, host: 'external' } },
			onSave: vi.fn()
		});
		expect(view.getByLabelText('Host')).toHaveProperty('value', 'my edit');
		await view.rerender({
			snapshot: {
				...snapshot,
				session_id: 'session-b',
				revision: 0,
				values: { ...snapshot.values, host: 'new session' }
			},
			onSave: vi.fn()
		});
		await waitFor(() => expect(view.getByLabelText('Host')).toHaveProperty('value', 'new session'));
	});

	it('clears the separate password draft only after explicit storage acknowledgement', async () => {
		const onSavePassword = vi.fn().mockResolvedValue('stored');
		const onSave = vi.fn().mockResolvedValue(saved(4));
		const view = render(ConfigurationView, { snapshot, onSave, onSavePassword });
		await fireEvent.change(view.getByLabelText('OBS password'), {
			target: { value: 'private secret' }
		});
		await fireEvent.click(view.getByRole('button', { name: 'Save password' }));
		await waitFor(() => expect(onSavePassword).toHaveBeenCalledOnce());
		expect(onSavePassword).toHaveBeenCalledWith({ password: 'private secret' });
		expect(onSave).not.toHaveBeenCalled();
		await waitFor(() => expect(view.getByLabelText('OBS password')).toHaveProperty('value', ''));
		expect(view.getByText('A password is stored. Enter a new one to replace it.')).toBeTruthy();
	});

	it('retains a failed password draft for a separate retry and returns to the editor on request', async () => {
		const onBack = vi.fn();
		const onSave = vi.fn();
		const onSavePassword = vi.fn().mockRejectedValue(
			new DesktopRequestError({
				code: 'credential_unavailable',
				message: 'Credential store unavailable.'
			})
		);
		const view = render(ConfigurationView, {
			snapshot,
			onSave,
			onSavePassword,
			onBack
		});
		await fireEvent.change(view.getByLabelText('OBS password'), { target: { value: 'keep me' } });
		await fireEvent.click(view.getByRole('button', { name: 'Save password' }));
		await waitFor(() =>
			expect(view.getByRole('alert').textContent).toContain('Credential store unavailable.')
		);
		await fireEvent.click(view.getByRole('button', { name: 'Dismiss save error' }));
		expect(view.queryByRole('alert')).toBeNull();
		expect(view.getByLabelText('OBS password')).toHaveProperty('value', 'keep me');
		expect(onSave).not.toHaveBeenCalled();
		await fireEvent.click(view.getByRole('button', { name: 'Save password' }));
		await waitFor(() => expect(onSavePassword).toHaveBeenCalledTimes(2));
		expect(onSavePassword).toHaveBeenNthCalledWith(2, { password: 'keep me' });
		await fireEvent.click(view.getByRole('button', { name: 'Back' }));
		expect(onBack).toHaveBeenCalledOnce();
	});

	it('keeps a newer password typed while the password save is pending', async () => {
		let finish!: (presence: 'stored') => void;
		const pending = new Promise<'stored'>((resolve) => {
			finish = resolve;
		});
		const onSavePassword = vi.fn().mockReturnValue(pending);
		const view = render(ConfigurationView, { snapshot, onSave: vi.fn(), onSavePassword });
		await fireEvent.change(view.getByLabelText('OBS password'), {
			target: { value: 'first secret' }
		});
		await fireEvent.click(view.getByRole('button', { name: 'Save password' }));
		await fireEvent.change(view.getByLabelText('OBS password'), {
			target: { value: 'newer secret' }
		});
		finish('stored');
		await waitFor(() =>
			expect(view.getByLabelText('OBS password')).toHaveProperty('value', 'newer secret')
		);
		expect(onSavePassword).toHaveBeenCalledWith({ password: 'first secret' });
	});
});
