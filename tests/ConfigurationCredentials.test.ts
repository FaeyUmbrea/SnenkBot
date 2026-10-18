import { render, fireEvent, waitFor, cleanup } from '@testing-library/svelte';
import { describe, it, expect, vi } from 'vitest';
import ConfigurationView from '../src/lib/ConfigurationView.svelte';
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
	values: { host: 'localhost' },
	authorization: null,
	authorization_ready: false,
	collection: null,
	credential: {
		id: 'obs-password',
		label: 'OBS password',
		emptyDescription: 'Optional password for the OBS connection.',
		storedDescription: 'A password is stored. Enter a new one to replace it.',
		unavailableDescription: 'Stored password status is unavailable.',
		saveLabel: 'Save password',
		savedMessage: 'OBS password saved.',
		saveError: 'The OBS password could not be saved.',
		removeLabel: null,
		removedMessage: null
	}
};
function props() {
	return {
		snapshot,
		credential: snapshot.credential!,
		onSave: vi.fn(async () => ({ snapshot, notices: [] })),
		onRefreshCredential: vi.fn<() => Promise<CredentialPresence>>().mockResolvedValue('stored'),
		onSaveCredential: vi.fn<() => Promise<CredentialPresence>>().mockResolvedValue('stored')
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
	it('reads cached presence once alongside generated configuration', async () => {
		const callbacks = props();
		const view = render(ConfigurationView, callbacks);
		await waitFor(() =>
			expect(view.getByText('A password is stored. Enter a new one to replace it.')).toBeTruthy()
		);
		expect((view.getByLabelText('Host') as HTMLInputElement).value).toBe('localhost');
		await view.rerender({ ...callbacks, snapshot: { ...snapshot, revision: 1 } });
		expect(callbacks.onRefreshCredential).toHaveBeenCalledTimes(1);
		expect(callbacks.onSaveCredential).not.toHaveBeenCalled();
	});
	it('retries failed status without exposing diagnostics or discarding typed drafts', async () => {
		const callbacks = props();
		callbacks.onRefreshCredential.mockRejectedValueOnce(new Error('/private/credential/backend'));
		const view = render(ConfigurationView, callbacks);
		await waitFor(() =>
			expect(view.getByRole('button', { name: 'Retry credential status' })).toBeTruthy()
		);
		await fireEvent.change(view.getByLabelText('Host'), { target: { value: 'stream-pc' } });
		await fireEvent.change(view.getByLabelText('OBS password'), {
			target: { value: 'typed-password' }
		});
		await fireEvent.click(view.getByRole('button', { name: 'Retry credential status' }));
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
		callbacks.onRefreshCredential.mockReturnValue(cache.promise);
		const view = render(ConfigurationView, callbacks);
		expect(view.getByText('Stored password status is unavailable.')).toBeTruthy();
		expect(view.queryByText('Optional password for the OBS connection.')).toBeNull();
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
		expect(callbacks.onSaveCredential).toHaveBeenCalledExactlyOnceWith('new-password');
	});
	it('ignores previous-session cache responses and stops after unmount', async () => {
		const old = deferred<CredentialPresence>();
		const callbacks = props();
		callbacks.onRefreshCredential.mockReturnValueOnce(old.promise).mockResolvedValue('stored');
		const view = render(ConfigurationView, callbacks);
		await view.rerender({ ...callbacks, snapshot: { ...snapshot, session_id: 'obs-two' } });
		await waitFor(() => expect(callbacks.onRefreshCredential).toHaveBeenCalledTimes(2));
		await waitFor(() =>
			expect(view.getByText('A password is stored. Enter a new one to replace it.')).toBeTruthy()
		);
		old.resolve('empty');
		await Promise.resolve();
		expect(view.getByText('A password is stored. Enter a new one to replace it.')).toBeTruthy();
		cleanup();
		expect(callbacks.onRefreshCredential).toHaveBeenCalledTimes(2);
	});
	it('failed explicit saves retain the stored indication and password draft', async () => {
		const callbacks = props();
		callbacks.onSaveCredential.mockRejectedValue(
			new DesktopRequestError({ code: 'unavailable', message: 'Could not save password.' })
		);
		const view = render(ConfigurationView, callbacks);
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
