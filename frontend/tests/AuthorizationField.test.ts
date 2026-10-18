import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import AuthorizationField from '../src/lib/AuthorizationField.svelte';
import { DesktopRequestError } from '../src/lib/desktop';
import type { AuthorizationDescriptor, CredentialPresence } from '../src/lib/contracts/index';

const descriptor: AuthorizationDescriptor = {
	id: 'studio-authorization',
	label: 'Studio authorization',
	description: 'Approve the connection in the companion application.',
	prerequisite_field: 'enabled',
	prerequisite_message: 'Enable and save the connection before authorizing.'
};

/** Builds a pairing field with overridable native operations. */
function props(
	overrides: Partial<{
		contextKey: string;
		ready: boolean;
		presence: () => Promise<CredentialPresence>;
		authorize: () => Promise<void>;
		forget: () => Promise<void>;
	}> = {}
) {
	return {
		descriptor,
		contextKey: overrides.contextKey ?? 'session-a',
		ready: overrides.ready ?? true,
		onPresence: overrides.presence ?? vi.fn().mockResolvedValue('empty'),
		onAuthorize: overrides.authorize ?? vi.fn().mockResolvedValue(undefined),
		onForget: overrides.forget ?? vi.fn().mockResolvedValue(undefined)
	};
}

describe('AuthorizationField', () => {
	it('reads presence and renders native-owned copy', async () => {
		const onPresence = vi.fn().mockResolvedValue('stored');
		const view = render(AuthorizationField, props({ presence: onPresence }));
		await waitFor(() => expect(view.getByText('Previously paired')).toBeTruthy());
		expect(onPresence).toHaveBeenCalledOnce();
		expect(view.getByText(descriptor.label)).toBeTruthy();
		expect(view.getByText(descriptor.description)).toBeTruthy();
	});

	it('retries an unavailable presence and uses native saved readiness', async () => {
		const onPresence = vi.fn().mockResolvedValueOnce('unavailable').mockResolvedValueOnce('empty');
		const onAuthorize = vi.fn();
		const view = render(
			AuthorizationField,
			props({ presence: onPresence, ready: false, authorize: onAuthorize })
		);
		await waitFor(() => expect(view.getByText('Authorization status unavailable')).toBeTruthy());
		await fireEvent.click(view.getByRole('button', { name: 'Check status' }));
		await waitFor(() => expect(view.getByText('Not authorized')).toBeTruthy());
		expect(onPresence).toHaveBeenCalledTimes(2);
		expect(view.getByRole('button', { name: 'Authorize' })).toHaveProperty('disabled', true);
		expect(view.getByText(descriptor.prerequisite_message!)).toBeTruthy();
		expect(onAuthorize).not.toHaveBeenCalled();
	});

	it('guards double authorization and verifies saved state before success', async () => {
		let finish!: () => void;
		const onAuthorize = vi.fn(
			() =>
				new Promise<void>((resolve) => {
					finish = resolve;
				})
		);
		const onPresence = vi.fn().mockResolvedValueOnce('empty').mockResolvedValueOnce('stored');
		const view = render(
			AuthorizationField,
			props({ authorize: onAuthorize, presence: onPresence })
		);
		await waitFor(() => expect(view.getByText('Not authorized')).toBeTruthy());
		await fireEvent.click(view.getByRole('button', { name: 'Authorize' }));
		await fireEvent.click(view.getByRole('button', { name: 'Working…' }));
		expect(onAuthorize).toHaveBeenCalledOnce();
		finish();
		await waitFor(() => expect(view.getByText(`${descriptor.label} is stored.`)).toBeTruthy());
		expect(view.getByText('Previously paired')).toBeTruthy();
	});

	it('uses safe request errors and hides unknown transport details', async () => {
		const native = new DesktopRequestError({
			code: 'unavailable',
			message: 'Connection service unavailable.'
		});
		const first = render(
			AuthorizationField,
			props({ authorize: vi.fn().mockRejectedValue(native) })
		);
		await waitFor(() => expect(first.getByText('Not authorized')).toBeTruthy());
		await fireEvent.click(first.getByRole('button', { name: 'Authorize' }));
		await waitFor(() =>
			expect(first.getByRole('alert').textContent).toContain('Connection service unavailable.')
		);
		await fireEvent.click(first.getByRole('button', { name: 'Dismiss error' }));
		expect(first.queryByRole('alert')).toBeNull();
		cleanup();
		const second = render(
			AuthorizationField,
			props({ authorize: vi.fn().mockRejectedValue(new Error('private detail')) })
		);
		await waitFor(() => expect(second.getByText('Not authorized')).toBeTruthy());
		await fireEvent.click(second.getByRole('button', { name: 'Authorize' }));
		await waitFor(() =>
			expect(second.getByRole('alert').textContent).toContain('Could not authorize.')
		);
		expect(second.queryByText('private detail')).toBeNull();
	});

	it('confirms removal and does not claim success without verification', async () => {
		const onPresence = vi.fn().mockResolvedValueOnce('stored').mockResolvedValueOnce('empty');
		const view = render(AuthorizationField, props({ presence: onPresence }));
		await waitFor(() => expect(view.getByText('Previously paired')).toBeTruthy());
		await fireEvent.click(view.getByRole('button', { name: 'Forget authorization' }));
		await waitFor(() => expect(view.getByText(`${descriptor.label} was removed.`)).toBeTruthy());
		cleanup();
		const unverified = render(
			AuthorizationField,
			props({ presence: vi.fn().mockResolvedValue('empty') })
		);
		await waitFor(() => expect(unverified.getByText('Not authorized')).toBeTruthy());
		await fireEvent.click(unverified.getByRole('button', { name: 'Authorize' }));
		await waitFor(() =>
			expect(unverified.getByRole('alert').textContent).toContain('could not be verified')
		);
	});

	it('ignores stale presence and authorization results after a context change', async () => {
		let finishPresence!: (value: CredentialPresence) => void;
		const onPresence = vi
			.fn()
			.mockReturnValueOnce(
				new Promise<CredentialPresence>((resolve) => {
					finishPresence = resolve;
				})
			)
			.mockResolvedValueOnce('empty');
		const view = render(AuthorizationField, props({ presence: onPresence }));
		await view.rerender(props({ contextKey: 'session-b', presence: onPresence }));
		await waitFor(() => expect(view.getByText('Not authorized')).toBeTruthy());
		finishPresence('stored');
		await Promise.resolve();
		expect(view.getByText('Not authorized')).toBeTruthy();
		cleanup();
		let finishAuthorization!: () => void;
		const onAuthorize = vi.fn(
			() =>
				new Promise<void>((resolve) => {
					finishAuthorization = resolve;
				})
		);
		const next = render(AuthorizationField, props({ authorize: onAuthorize }));
		await waitFor(() => expect(next.getByText('Not authorized')).toBeTruthy());
		await fireEvent.click(next.getByRole('button', { name: 'Authorize' }));
		await next.rerender(props({ contextKey: 'session-c', authorize: onAuthorize }));
		finishAuthorization();
		await Promise.resolve();
		expect(next.queryByText(`${descriptor.label} is stored.`)).toBeNull();
	});

	it('ignores presence after unmount', async () => {
		let finish!: (value: CredentialPresence) => void;
		const view = render(
			AuthorizationField,
			props({
				presence: () =>
					new Promise<CredentialPresence>((resolve) => {
						finish = resolve;
					})
			})
		);
		view.unmount();
		finish('stored');
		await Promise.resolve();
	});
});
