import { render, fireEvent, waitFor, cleanup } from '@testing-library/svelte';
import { describe, it, expect, vi } from 'vitest';
import ObsConnectionView from '../src/lib/ObsConnectionView.svelte';
import { DesktopRequestError } from '../src/lib/desktop';
import type { ConfigurationSnapshot, CredentialPresence } from '../src/lib/contracts/index';

const snapshot: ConfigurationSnapshot = {
	session_id: 'obs-one',
	revision: 0,
	module: 'obs',
	schema: {
		id: 'obs.connection',
		title: 'OBS Studio',
		version: 1,
		outputs: [],
		fields: [
			{
				id: 'host',
				label: 'Host',
				description: '',
				introduced_in: 1,
				kind: 'text',
				required: true,
				choice_source: null
			}
		]
	},
	defaults: { host: 'localhost' },
	values: { host: 'localhost' }
};
function props() {
	return {
		snapshot,
		connections: [],
		workflows: [],
		onOpen: vi.fn(),
		onSave: vi.fn(async () => ({ snapshot, notices: [] })),
		onPresence: vi.fn<() => Promise<CredentialPresence>>().mockResolvedValue('stored'),
		onSavePassword: vi.fn<() => Promise<CredentialPresence>>().mockResolvedValue('stored')
	};
}
function deferred<T>() {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((done) => {
		resolve = done;
	});
	return { promise, resolve };
}

describe('OBS connection settings', () => {
	it('reads cached presence once and composes generated configuration and usage', async () => {
		const callbacks = props();
		const view = render(ObsConnectionView, callbacks);
		await waitFor(() =>
			expect(view.getByText('A password is stored. Enter a new one to replace it.')).toBeTruthy()
		);
		expect((view.getByLabelText('Host') as HTMLInputElement).value).toBe('localhost');
		expect(view.getByRole('heading', { name: 'Used by' })).toBeTruthy();
		await view.rerender({ ...callbacks, snapshot: { ...snapshot, revision: 1 } });
		expect(callbacks.onPresence).toHaveBeenCalledTimes(1);
		expect(callbacks.onSavePassword).not.toHaveBeenCalled();
	});
	it('retries failed status without exposing diagnostics or discarding typed drafts', async () => {
		const callbacks = props();
		callbacks.onPresence.mockRejectedValueOnce(new Error('/private/credential/backend'));
		const view = render(ObsConnectionView, callbacks);
		await waitFor(() =>
			expect(view.getByRole('button', { name: 'Retry password status' })).toBeTruthy()
		);
		await fireEvent.change(view.getByLabelText('Host'), { target: { value: 'stream-pc' } });
		await fireEvent.change(view.getByLabelText('OBS password'), {
			target: { value: 'typed-password' }
		});
		await fireEvent.click(view.getByRole('button', { name: 'Retry password status' }));
		await waitFor(() =>
			expect(view.getByText('A password is stored. Enter a new one to replace it.')).toBeTruthy()
		);
		expect((view.getByLabelText('Host') as HTMLInputElement).value).toBe('stream-pc');
		expect((view.getByLabelText('OBS password') as HTMLInputElement).value).toBe('typed-password');
		expect(view.container.textContent).not.toContain('/private/');
	});
	it('password confirmation supersedes an older presence read', async () => {
		const cache = deferred<CredentialPresence>();
		const callbacks = props();
		callbacks.onPresence.mockReturnValue(cache.promise);
		const view = render(ObsConnectionView, callbacks);
		await fireEvent.change(view.getByLabelText('OBS password'), {
			target: { value: 'new-password' }
		});
		await fireEvent.click(view.getByRole('button', { name: 'Save password' }));
		await waitFor(() =>
			expect(view.getByText('A password is stored. Enter a new one to replace it.')).toBeTruthy()
		);
		cache.resolve('empty');
		await Promise.resolve();
		expect(view.getByText('A password is stored. Enter a new one to replace it.')).toBeTruthy();
		expect(callbacks.onSavePassword).toHaveBeenCalledExactlyOnceWith({ password: 'new-password' });
	});
	it('ignores previous-session cache responses and stops after unmount', async () => {
		const old = deferred<CredentialPresence>();
		const callbacks = props();
		callbacks.onPresence.mockReturnValueOnce(old.promise).mockResolvedValue('stored');
		const view = render(ObsConnectionView, callbacks);
		await view.rerender({ ...callbacks, snapshot: { ...snapshot, session_id: 'obs-two' } });
		await waitFor(() => expect(callbacks.onPresence).toHaveBeenCalledTimes(2));
		await waitFor(() =>
			expect(view.getByText('A password is stored. Enter a new one to replace it.')).toBeTruthy()
		);
		old.resolve('empty');
		await Promise.resolve();
		expect(view.getByText('A password is stored. Enter a new one to replace it.')).toBeTruthy();
		cleanup();
		expect(callbacks.onPresence).toHaveBeenCalledTimes(2);
	});
	it('failed explicit saves retain the stored indication and password draft', async () => {
		const callbacks = props();
		callbacks.onSavePassword.mockRejectedValue(
			new DesktopRequestError({ code: 'unavailable', message: 'Could not save password.' })
		);
		const view = render(ObsConnectionView, callbacks);
		await waitFor(() =>
			expect(view.getByText('A password is stored. Enter a new one to replace it.')).toBeTruthy()
		);
		await fireEvent.change(view.getByLabelText('OBS password'), {
			target: { value: 'replacement' }
		});
		await fireEvent.click(view.getByRole('button', { name: 'Save password' }));
		await waitFor(() => expect(view.getByText('Could not save password.')).toBeTruthy());
		expect((view.getByLabelText('OBS password') as HTMLInputElement).value).toBe('replacement');
		expect(view.getByText('A password is stored. Enter a new one to replace it.')).toBeTruthy();
	});
});
