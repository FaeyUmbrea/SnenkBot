import { fireEvent, render, waitFor } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import AccountConnectionsView from '../src/lib/AccountConnectionsView.svelte';
import { DesktopRequestError } from '../src/lib/desktop';
import type {
	AccountAuthentication,
	AccountConnections
} from '../src/lib/AccountConnectionsView.svelte';

const accounts: AccountConnections = {
	integration_id: 'sample',
	title: 'Sample service',
	account_selection_hint: 'Use a private window to choose another account.',
	roles: [
		{
			descriptor: {
				id: 'owner',
				label: 'Channel owner',
				description: 'Receives channel activity.',
				optional: false,
				empty_name: 'No account connected',
				empty_status: 'Not signed in',
				help: null
			},
			account: { user_id: '1', login: 'owner_login' }
		},
		{
			descriptor: {
				id: 'helper',
				label: 'Helper account',
				description: 'Sends messages.',
				optional: true,
				empty_name: 'No separate helper account',
				empty_status: 'Uses the owner account',
				help: 'A separate helper is optional.'
			},
			account: null
		}
	]
};
const code: AccountAuthentication = {
	attempt_id: 'attempt-a',
	role: 'helper',
	phase: { state: 'code', data: { url: 'https://example.test/activate', code: 'ABCD1234' } }
};

/** Supplies isolated callbacks for one generic account-control render. */
function props(authentication: AccountAuthentication | null = null) {
	return {
		accounts,
		authentication,
		onStart: vi.fn<(roleId: string) => Promise<void>>(async () => {}),
		onApprove: vi.fn<(attemptId: string, userId: string) => Promise<void>>(async () => {}),
		onCancel: vi.fn<(attemptId: string) => Promise<void>>(async () => {}),
		onOpen: vi.fn<(attemptId: string) => Promise<void>>(async () => {}),
		onCopy: vi.fn<(attemptId: string, target: 'link' | 'code') => Promise<void>>(async () => {})
	};
}

/** Exposes a controllable request to exercise late native responses. */
function deferred<T = void>() {
	let resolve!: (value: T) => void;
	let reject!: (reason: unknown) => void;
	const promise = new Promise<T>((finish, fail) => {
		resolve = finish;
		reject = fail;
	});
	return { promise, resolve, reject };
}

describe('AccountConnectionsView', () => {
	it('renders declared roles and selects one without a fixed role list', async () => {
		const options = props();
		const view = render(AccountConnectionsView, { ...options, selectedRole: 'helper' });
		expect(view.queryByText('owner_login')).toBeNull();
		expect(view.getByText('Helper account · optional')).toBeTruthy();
		expect(view.getByText('No separate helper account')).toBeTruthy();
		expect(view.getByText('Uses the owner account')).toBeTruthy();
		await fireEvent.click(view.getByRole('button', { name: 'Connect helper account' }));
		expect(options.onStart).toHaveBeenCalledExactlyOnceWith('helper');
	});

	it('uses connection status only for stored accounts and retains native help', () => {
		const view = render(AccountConnectionsView, {
			...props(),
			connections: [
				{
					integration: 'sample',
					connection: 'owner',
					title: 'Owner',
					state: 'error' as const,
					show_when_healthy: true,
					state_label: 'Needs attention',
					detail: 'Needs reconnection'
				},
				{
					integration: 'sample',
					connection: 'helper',
					title: 'Helper',
					state: 'error' as const,
					show_when_healthy: true,
					state_label: 'Needs attention',
					detail: 'Unavailable'
				}
			],
			requests: [
				{ integration: 'sample', connection: 'helper', reason: 'Reconnect', affected_features: [] }
			]
		});
		expect(view.getByText('Needs reconnection')).toBeTruthy();
		expect(view.getByText('Uses the owner account')).toBeTruthy();
		expect(view.getByText('A separate helper is optional.')).toBeTruthy();
		expect(view.queryByText('Unavailable')).toBeNull();
	});

	it('binds device-code actions to the current attempt and uses native guidance', async () => {
		const options = props(code);
		const view = render(AccountConnectionsView, options);
		expect(view.getByLabelText('Activation code')).toHaveProperty('value', 'ABCD1234');
		expect(view.getByText(/Use a private window/)).toBeTruthy();
		await fireEvent.click(view.getByRole('button', { name: 'Copy link' }));
		await fireEvent.click(view.getByRole('button', { name: 'Copy code' }));
		await fireEvent.click(view.getByRole('button', { name: 'Open in browser' }));
		expect(options.onCopy.mock.calls).toEqual([
			['attempt-a', 'link'],
			['attempt-a', 'code']
		]);
		expect(options.onOpen).toHaveBeenCalledExactlyOnceWith('attempt-a');
	});

	it('holds approval until phase change and drops feedback from an earlier attempt', async () => {
		const review: AccountAuthentication = {
			...code,
			phase: { state: 'review', data: { login: 'helper_login', user_id: '2' } }
		};
		const options = props(review);
		const view = render(AccountConnectionsView, options);
		const confirm = view.getByRole('button', { name: 'Confirm account' });
		await fireEvent.click(confirm);
		await fireEvent.click(confirm);
		expect(options.onApprove).toHaveBeenCalledExactlyOnceWith('attempt-a', '2');
		expect(confirm).toHaveProperty('disabled', true);
		await view.rerender({ ...options, authentication: code });
		await waitFor(() => expect(view.queryByText('Confirm account')).toBeNull());
	});

	it('keeps both connect actions guarded while the backend starts an attempt', async () => {
		const pending = deferred();
		const options = props();
		options.onStart.mockReturnValue(pending.promise);
		const view = render(AccountConnectionsView, options);
		await fireEvent.click(view.getByRole('button', { name: 'Connect helper account' }));
		await fireEvent.click(view.getByRole('button', { name: 'Reconnect channel owner' }));
		expect(options.onStart).toHaveBeenCalledExactlyOnceWith('helper');
		expect(view.getByRole('button', { name: 'Reconnect channel owner' })).toHaveProperty(
			'disabled',
			true
		);
		pending.resolve();
		await waitFor(() =>
			expect(view.getByRole('button', { name: 'Connect helper account' })).toHaveProperty(
				'disabled',
				false
			)
		);
		expect(view.queryByLabelText('Activation code')).toBeNull();
		await view.rerender({ ...options, authentication: code });
		expect(view.getByLabelText('Activation code')).toHaveProperty('value', 'ABCD1234');
	});

	it('retains the stored identity through reconnect and ignores a delayed start after review', async () => {
		const pending = deferred();
		const options = props();
		options.onStart.mockReturnValue(pending.promise);
		const view = render(AccountConnectionsView, options);
		await fireEvent.click(view.getByRole('button', { name: 'Reconnect channel owner' }));
		const review: AccountAuthentication = {
			...code,
			role: 'owner',
			phase: { state: 'review', data: { login: 'replacement', user_id: '2' } }
		};
		await view.rerender({ ...options, authentication: review });
		pending.resolve();
		await waitFor(() => expect(view.getByText('replacement')).toBeTruthy());
		expect(view.getByText('owner_login')).toBeTruthy();
		expect(view.queryByLabelText('Activation code')).toBeNull();
		await fireEvent.click(view.getByRole('button', { name: 'Try another account' }));
		expect(options.onStart).toHaveBeenCalledTimes(2);
		expect(options.onApprove).not.toHaveBeenCalled();
	});

	it.each(['open', 'copy', 'cancel', 'approve', 'start'] as const)(
		'ignores stale %s failures when a native attempt changes',
		async (operation) => {
			const pending = deferred();
			const review: AccountAuthentication = {
				...code,
				phase: { state: 'review', data: { login: 'replacement', user_id: '2' } }
			};
			const options = props(operation === 'approve' ? review : code);
			const callback = {
				open: options.onOpen,
				copy: options.onCopy,
				cancel: options.onCancel,
				approve: options.onApprove,
				start: options.onStart
			}[operation];
			callback.mockReturnValue(pending.promise);
			const view = render(AccountConnectionsView, options);
			const name = {
				open: 'Open in browser',
				copy: 'Copy link',
				cancel: 'Cancel',
				approve: 'Confirm account',
				start: 'Connect helper account'
			}[operation];
			await fireEvent.click(view.getByRole('button', { name }));
			await view.rerender({ ...options, authentication: { ...code, attempt_id: 'attempt-b' } });
			pending.reject(new DesktopRequestError({ code: 'unavailable', message: 'Old failure' }));
			await waitFor(() =>
				expect(view.getByRole('button', { name: 'Cancel' })).toHaveProperty('disabled', false)
			);
			expect(view.queryByRole('alert')).toBeNull();
		}
	);

	it('does not let an old successful copy release the replacement attempt guard', async () => {
		const old = deferred();
		const current = deferred();
		const options = props(code);
		options.onCopy.mockReturnValueOnce(old.promise).mockReturnValueOnce(current.promise);
		const view = render(AccountConnectionsView, options);
		await fireEvent.click(view.getByRole('button', { name: 'Copy link' }));
		await view.rerender({ ...options, authentication: { ...code, attempt_id: 'attempt-b' } });
		await fireEvent.click(view.getByRole('button', { name: 'Copy link' }));
		old.resolve();
		await waitFor(() =>
			expect(view.getByRole('button', { name: 'Copying…' })).toHaveProperty('disabled', true)
		);
		expect(view.queryByText('Link copied.')).toBeNull();
		expect(options.onCopy).toHaveBeenLastCalledWith('attempt-b', 'link');
		current.resolve();
		await waitFor(() => expect(view.getByText('Link copied.')).toBeTruthy());
	});

	it('keeps cancellation under backend control and hides internal errors', async () => {
		const options = props(code);
		options.onOpen.mockRejectedValue(new Error('secret internal detail'));
		options.onCancel.mockRejectedValueOnce(
			new DesktopRequestError({ code: 'unavailable', message: 'Cancellation unavailable.' })
		);
		const view = render(AccountConnectionsView, options);
		await fireEvent.click(view.getByRole('button', { name: 'Open in browser' }));
		await waitFor(() =>
			expect(view.getByRole('alert').textContent).toBe(
				'Could not complete this account request. Try again.'
			)
		);
		expect(view.queryByText('secret internal detail')).toBeNull();
		await fireEvent.click(view.getByRole('button', { name: 'Cancel' }));
		await waitFor(() =>
			expect(view.getByRole('alert').textContent).toBe('Cancellation unavailable.')
		);
		await fireEvent.click(view.getByRole('button', { name: 'Cancel' }));
		expect(view.getByRole('button', { name: 'Cancelling…' })).toHaveProperty('disabled', true);
		expect(view.getByLabelText('Activation code')).toBeTruthy();
		await view.rerender({ ...options, authentication: { ...code, phase: { state: 'cancelled' } } });
		expect(view.queryByLabelText('Activation code')).toBeNull();
	});

	it('supports cancelling during start and guards duplicate requests through unmount', async () => {
		const options = props({ ...code, phase: { state: 'starting' } });
		const view = render(AccountConnectionsView, options);
		await fireEvent.click(view.getByRole('button', { name: 'Cancel' }));
		expect(options.onCancel).toHaveBeenCalledExactlyOnceWith('attempt-a');
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
		view.unmount();
		opening.reject(new Error('late failure'));
		copying.resolve();
	});
});
