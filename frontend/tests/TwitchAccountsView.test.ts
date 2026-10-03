import { fireEvent, render, waitFor } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import TwitchAccountsView from '../src/lib/TwitchAccountsView.svelte';
import { DesktopRequestError } from '../src/lib/desktop';
import type { TwitchAuthentication } from '../src/lib/contracts/index';

const code: TwitchAuthentication = {
	attempt_id: 'attempt-a',
	role: 'broadcaster',
	phase: { state: 'code', data: { url: 'https://www.twitch.tv/activate', code: 'ABCD1234' } }
};
const review: TwitchAuthentication = {
	...code,
	phase: { state: 'review', data: { login: 'reviewed_account', user_id: 'user-2' } }
};

function props(authentication: TwitchAuthentication | null = null) {
	return {
		accounts: { broadcaster: { user_id: 'user-1', login: 'current_account' }, bot: null },
		authentication,
		onStart: vi.fn().mockResolvedValue(code),
		onApprove: vi.fn().mockResolvedValue(undefined),
		onCancel: vi.fn().mockResolvedValue(undefined),
		onOpen: vi.fn().mockResolvedValue(undefined),
		onCopy: vi.fn().mockResolvedValue(undefined)
	};
}

function deferred<T = void>() {
	let resolve!: (value: T) => void;
	let reject!: (reason: unknown) => void;
	const promise = new Promise<T>((finish, fail) => {
		resolve = finish;
		reject = fail;
	});
	return { promise, resolve, reject };
}

describe('TwitchAccountsView', () => {
	it('shows a connected broadcaster and optional bot without treating the empty bot as an error', () => {
		const options = props();
		const view = render(TwitchAccountsView, {
			...options,
			connections: [
				{
					integration: 'twitch',
					connection: 'bot',
					title: 'Bot',
					state: 'error',
					detail: 'Bot unavailable'
				}
			],
			requests: [
				{ integration: 'twitch', connection: 'bot', reason: 'Reconnect bot', affected_features: [] }
			]
		});
		expect(view.getByText('current_account')).toBeTruthy();
		expect(view.getByText('Bot account · optional')).toBeTruthy();
		expect(view.getByText('Uses your broadcaster account')).toBeTruthy();
		expect(view.queryByText('Bot unavailable')).toBeNull();
		expect(view.queryByText('Reconnect bot')).toBeNull();
		expect(view.queryByRole('alert')).toBeNull();
		expect(options.onStart).not.toHaveBeenCalled();
		expect(options.onOpen).not.toHaveBeenCalled();
	});

	it('starts only the chosen role, guards both start buttons, and waits for backend progress', async () => {
		const pending = deferred<TwitchAuthentication>();
		const options = props();
		options.onStart.mockReturnValue(pending.promise);
		const view = render(TwitchAccountsView, options);
		await fireEvent.click(view.getByRole('button', { name: 'Connect bot account' }));
		await fireEvent.click(view.getByRole('button', { name: 'Reconnect broadcaster account' }));
		expect(options.onStart).toHaveBeenCalledExactlyOnceWith({ role: 'bot' });
		expect(view.getByRole('button', { name: 'Reconnect broadcaster account' })).toHaveProperty(
			'disabled',
			true
		);
		pending.resolve({ ...code, role: 'bot' });
		await waitFor(() =>
			expect(view.getByRole('button', { name: 'Connect bot account' })).toHaveProperty(
				'disabled',
				false
			)
		);
		expect(view.queryByLabelText('Activation code')).toBeNull();
		expect(options.onOpen).not.toHaveBeenCalled();
		await view.rerender({ ...options, authentication: { ...code, role: 'bot' } });
		expect(view.getByLabelText('Activation code')).toHaveProperty('value', 'ABCD1234');
	});

	it('keeps existing identity visible during reconnect and explicitly copies or opens the same attempt', async () => {
		const options = props(code);
		const view = render(TwitchAccountsView, options);
		expect(view.getByText('current_account')).toBeTruthy();
		expect(view.getByLabelText('Activation link')).toHaveProperty('readOnly', true);
		expect(view.getByLabelText('Activation code')).toHaveProperty('readOnly', true);
		expect(view.getByText(/private or incognito window/)).toBeTruthy();
		expect(options.onOpen).not.toHaveBeenCalled();
		await fireEvent.click(view.getByRole('button', { name: 'Copy link' }));
		await waitFor(() => expect(view.getByText('Link copied.')).toBeTruthy());
		await fireEvent.click(view.getByRole('button', { name: 'Copy code' }));
		await waitFor(() => expect(view.getByText('Code copied.')).toBeTruthy());
		await fireEvent.click(view.getByRole('button', { name: 'Open in browser' }));
		expect(options.onCopy.mock.calls).toEqual([
			[{ attempt_id: 'attempt-a', target: 'link' }],
			[{ attempt_id: 'attempt-a', target: 'code' }]
		]);
		expect(options.onOpen).toHaveBeenCalledExactlyOnceWith({ attempt_id: 'attempt-a' });
		expect(options.onStart).not.toHaveBeenCalled();
	});

	it('shows the reviewed account and role before confirming the reviewed user identity once', async () => {
		const options = props(review);
		const view = render(TwitchAccountsView, options);
		expect(view.getByText('reviewed_account')).toBeTruthy();
		expect(view.getByText('broadcaster account', { selector: 'strong' })).toBeTruthy();
		expect(view.getByText('current_account')).toBeTruthy();
		await fireEvent.click(view.getByRole('button', { name: 'Confirm account' }));
		await fireEvent.click(view.getByRole('button', { name: 'Saving…' }));
		expect(options.onApprove).toHaveBeenCalledExactlyOnceWith({
			attempt_id: 'attempt-a',
			user_id: 'user-2'
		});
		expect(view.getByRole('button', { name: 'Try another account' })).toHaveProperty(
			'disabled',
			true
		);
		expect(view.getByRole('button', { name: 'Cancel' })).toHaveProperty('disabled', true);
		await view.rerender({ ...options, authentication: { ...review, phase: { state: 'saving' } } });
		expect(view.getByText('Saving your broadcaster account…')).toBeTruthy();
		expect(view.queryByRole('button', { name: 'Cancel' })).toBeNull();
		expect(view.getByRole('button', { name: 'Connect bot account' })).toHaveProperty(
			'disabled',
			true
		);
		await view.rerender({
			...options,
			accounts: { broadcaster: { user_id: 'user-2', login: 'reviewed_account' }, bot: null },
			authentication: {
				...review,
				phase: { state: 'connected', data: { login: 'reviewed_account' } }
			}
		});
		expect(view.queryByText('current_account')).toBeNull();
		expect(view.getByText('reviewed_account')).toBeTruthy();
	});

	it('permits another account from review without approving it', async () => {
		const options = props(review);
		const view = render(TwitchAccountsView, options);
		await fireEvent.click(view.getByRole('button', { name: 'Try another account' }));
		expect(options.onStart).toHaveBeenCalledExactlyOnceWith({ role: 'broadcaster' });
		expect(options.onApprove).not.toHaveBeenCalled();
		expect(view.getByText('reviewed_account')).toBeTruthy();
	});

	it('ignores a delayed start result after the feed has advanced to review', async () => {
		const pending = deferred<TwitchAuthentication>();
		const options = props();
		options.onStart.mockReturnValue(pending.promise);
		const view = render(TwitchAccountsView, options);
		await fireEvent.click(view.getByRole('button', { name: 'Reconnect broadcaster account' }));
		await view.rerender({ ...options, authentication: review });
		pending.resolve(code);
		await waitFor(() => expect(view.getByText('reviewed_account')).toBeTruthy());
		expect(view.queryByLabelText('Activation code')).toBeNull();
	});

	it.each(['open', 'copy', 'cancel', 'approve', 'start'] as const)(
		'ignores stale %s failures after the attempt is replaced',
		async (operation) => {
			const pending = deferred();
			const options = props(operation === 'approve' ? review : code);
			const callback = {
				open: options.onOpen,
				copy: options.onCopy,
				cancel: options.onCancel,
				approve: options.onApprove,
				start: options.onStart
			}[operation];
			callback.mockReturnValue(pending.promise);
			const view = render(TwitchAccountsView, options);
			const name = {
				open: 'Open in browser',
				copy: 'Copy link',
				cancel: 'Cancel',
				approve: 'Confirm account',
				start: 'Reconnect broadcaster account'
			}[operation];
			await fireEvent.click(view.getByRole('button', { name }));
			await view.rerender({ ...options, authentication: { ...code, attempt_id: 'attempt-b' } });
			pending.reject(new DesktopRequestError({ code: 'unavailable', message: 'Old failure' }));
			await waitFor(() =>
				expect(view.getByRole('button', { name: 'Cancel' })).toHaveProperty('disabled', false)
			);
			expect(view.queryByRole('alert')).toBeNull();
			expect(view.queryByText('Old failure')).toBeNull();
		}
	);

	it('ignores an old successful copy without releasing the replacement copy guard', async () => {
		const old = deferred();
		const current = deferred();
		const options = props(code);
		options.onCopy.mockReturnValueOnce(old.promise).mockReturnValueOnce(current.promise);
		const view = render(TwitchAccountsView, options);
		await fireEvent.click(view.getByRole('button', { name: 'Copy link' }));
		await view.rerender({ ...options, authentication: { ...code, attempt_id: 'attempt-b' } });
		await fireEvent.click(view.getByRole('button', { name: 'Copy link' }));
		old.resolve();
		await waitFor(() =>
			expect(view.getByRole('button', { name: 'Copying…' })).toHaveProperty('disabled', true)
		);
		expect(view.queryByText('Link copied.')).toBeNull();
		expect(options.onCopy).toHaveBeenLastCalledWith({ attempt_id: 'attempt-b', target: 'link' });
		current.resolve();
		await waitFor(() => expect(view.getByText('Link copied.')).toBeTruthy());
	});

	it('handles failed or expired sign-in with retry and no terminal cancellation', async () => {
		const options = props({
			...code,
			phase: { state: 'failed', data: { message: 'This code expired. Try again.' } }
		});
		const view = render(TwitchAccountsView, options);
		expect(view.getByRole('alert').textContent).toBe('This code expired. Try again.');
		await fireEvent.click(view.getByRole('button', { name: 'Try again' }));
		expect(options.onStart).toHaveBeenCalledExactlyOnceWith({ role: 'broadcaster' });
		expect(view.queryByRole('button', { name: 'Cancel' })).toBeNull();
		expect(options.onCancel).not.toHaveBeenCalled();
	});

	it('uses safe request feedback, allows retry, and keeps cancellation backend controlled', async () => {
		const options = props(code);
		options.onOpen.mockRejectedValue(new Error('secret internal detail'));
		options.onCancel.mockRejectedValueOnce(
			new DesktopRequestError({
				code: 'unavailable',
				message: 'Sign-in could not be cancelled. Try again.'
			})
		);
		const view = render(TwitchAccountsView, options);
		await fireEvent.click(view.getByRole('button', { name: 'Open in browser' }));
		await waitFor(() =>
			expect(view.getByRole('alert').textContent).toBe(
				'Could not complete this Twitch request. Try again.'
			)
		);
		expect(view.queryByText('secret internal detail')).toBeNull();
		await fireEvent.click(view.getByRole('button', { name: 'Cancel' }));
		await waitFor(() =>
			expect(view.getByRole('alert').textContent).toBe('Sign-in could not be cancelled. Try again.')
		);
		await fireEvent.click(view.getByRole('button', { name: 'Cancel' }));
		await waitFor(() =>
			expect(view.getByRole('button', { name: 'Cancelling…' })).toHaveProperty('disabled', true)
		);
		expect(view.getByLabelText('Activation code')).toBeTruthy();
		await view.rerender({ ...options, authentication: { ...code, phase: { state: 'cancelled' } } });
		expect(view.queryByLabelText('Activation code')).toBeNull();
	});

	it('keeps cancellation available during starting and guards duplicate browser/copy requests', async () => {
		const options = props({ ...code, phase: { state: 'starting' } });
		const view = render(TwitchAccountsView, options);
		await fireEvent.click(view.getByRole('button', { name: 'Cancel' }));
		expect(options.onCancel).toHaveBeenCalledExactlyOnceWith({ attempt_id: 'attempt-a' });
		await view.rerender({ ...options, authentication: { ...code, attempt_id: 'attempt-b' } });
		const opening = deferred();
		const copying = deferred();
		options.onOpen.mockReturnValue(opening.promise);
		options.onCopy.mockReturnValue(copying.promise);
		await fireEvent.click(view.getByRole('button', { name: 'Open in browser' }));
		await fireEvent.click(view.getByRole('button', { name: 'Opening…' }));
		await fireEvent.click(view.getByRole('button', { name: 'Copy code' }));
		await fireEvent.click(view.getByRole('button', { name: 'Copying…' }));
		expect(options.onOpen).toHaveBeenCalledOnce();
		expect(options.onCopy).toHaveBeenCalledOnce();
		await view.unmount();
		opening.reject(new Error('late failure'));
		copying.resolve();
	});
});
