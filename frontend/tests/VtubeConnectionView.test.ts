import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import VtubeConnectionView from '../src/lib/VtubeConnectionView.svelte';
import { DesktopRequestError } from '../src/lib/desktop';
import type {
	ConfigurationSaveResult,
	ConfigurationSnapshot,
	CredentialPresence
} from '../src/lib/contracts/index';

const snapshot: ConfigurationSnapshot = {
	session_id: 'session-a',
	revision: 2,
	module: 'vtube_studio',
	schema: {
		id: 'vtube_studio',
		version: 1,
		title: 'VTube Studio',
		outputs: [],
		fields: [
			{
				id: 'host',
				label: 'Server',
				description: '',
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
			}
		]
	},
	defaults: { host: 'localhost', port: 8001, enabled: true },
	values: { host: 'localhost', port: 8001, enabled: true }
};

function props(
	overrides: Partial<{
		presence: () => Promise<CredentialPresence>;
		authorize: () => Promise<void>;
		forget: () => Promise<void>;
		snapshot: ConfigurationSnapshot;
	}> = {}
) {
	return {
		snapshot: overrides.snapshot ?? snapshot,
		connections: [],
		workflows: [],
		onOpen: vi.fn(),
		onSave: vi.fn(
			async ({
				values
			}: {
				values: Record<string, unknown>;
			}): Promise<ConfigurationSaveResult> => ({
				snapshot: { ...snapshot, values },
				notices: []
			})
		),
		onPresence: overrides.presence ?? vi.fn().mockResolvedValue('empty'),
		onAuthorize: overrides.authorize ?? vi.fn().mockResolvedValue(undefined),
		onForget: overrides.forget ?? vi.fn().mockResolvedValue(undefined)
	};
}

describe('VtubeConnectionView', () => {
	it('reads only cache presence on mount and keeps generated configuration fields', async () => {
		const onPresence = vi.fn().mockResolvedValue('stored');
		const view = render(VtubeConnectionView, props({ presence: onPresence }));
		await waitFor(() => expect(view.getByText('Previously paired')).toBeTruthy());
		expect(onPresence).toHaveBeenCalledOnce();
		expect(view.getByLabelText('Server')).toHaveProperty('value', 'localhost');
		expect(view.getByLabelText('Port')).toHaveProperty('value', '8001');
		expect(view.getByLabelText('Enabled')).toHaveProperty('checked', true);
		expect(view.getByText(/VTube Studio will ask you to approve/)).toBeTruthy();
	});

	it('offers an explicit retry after presence becomes unavailable', async () => {
		const onPresence = vi.fn().mockResolvedValueOnce('unavailable').mockResolvedValueOnce('empty');
		const view = render(VtubeConnectionView, props({ presence: onPresence }));
		await waitFor(() => expect(view.getByText('Authorization status unavailable')).toBeTruthy());
		await fireEvent.click(view.getByRole('button', { name: 'Check status' }));
		await waitFor(() => expect(view.getByText('Not authorized')).toBeTruthy());
		expect(onPresence).toHaveBeenCalledTimes(2);
	});

	it('blocks authorization until the setting is enabled and saved', async () => {
		const onAuthorize = vi.fn();
		const disabled = { ...snapshot, values: { ...snapshot.values, enabled: false } };
		const view = render(VtubeConnectionView, props({ snapshot: disabled, authorize: onAuthorize }));
		await waitFor(() => expect(view.getByText('Not authorized')).toBeTruthy());
		await fireEvent.click(view.getByRole('button', { name: 'Authorize' }));
		expect(onAuthorize).not.toHaveBeenCalled();
		expect(view.getByRole('button', { name: 'Authorize' })).toHaveProperty('disabled', true);
		expect(
			view.getAllByText('Enable and save VTube Studio settings before authorizing.')[0]
		).toBeTruthy();
	});

	it('guards double authorization clicks and reports success only after cache verification', async () => {
		let finish!: () => void;
		const authorize = vi.fn(
			() =>
				new Promise<void>((resolve) => {
					finish = resolve;
				})
		);
		const onPresence = vi.fn().mockResolvedValueOnce('empty').mockResolvedValueOnce('stored');
		const view = render(VtubeConnectionView, props({ authorize, presence: onPresence }));
		await waitFor(() => expect(view.getByText('Not authorized')).toBeTruthy());
		await fireEvent.click(view.getByRole('button', { name: 'Authorize' }));
		await fireEvent.click(view.getByRole('button', { name: 'Working…' }));
		expect(authorize).toHaveBeenCalledOnce();
		finish();
		await waitFor(() =>
			expect(view.getByText('VTube Studio authorization is stored.')).toBeTruthy()
		);
		expect(view.getByText('Previously paired')).toBeTruthy();
	});

	it('shows safe request errors and generic text for unknown failures, with dismissible alerts', async () => {
		const requestFailure = new DesktopRequestError({
			code: 'unavailable',
			message: 'Connection service unavailable.'
		});
		const first = render(
			VtubeConnectionView,
			props({ authorize: vi.fn().mockRejectedValue(requestFailure) })
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
			VtubeConnectionView,
			props({ authorize: vi.fn().mockRejectedValue(new Error('private detail')) })
		);
		await waitFor(() => expect(second.getByText('Not authorized')).toBeTruthy());
		await fireEvent.click(second.getByRole('button', { name: 'Authorize' }));
		await waitFor(() =>
			expect(second.getByRole('alert').textContent).toContain('Could not authorize VTube Studio.')
		);
		expect(second.queryByText('private detail')).toBeNull();
	});

	it('verifies removal before claiming authorization was forgotten', async () => {
		const onPresence = vi.fn().mockResolvedValueOnce('stored').mockResolvedValueOnce('empty');
		const view = render(VtubeConnectionView, props({ presence: onPresence }));
		await waitFor(() => expect(view.getByText('Previously paired')).toBeTruthy());
		await fireEvent.click(view.getByRole('button', { name: 'Forget authorization' }));
		await waitFor(() =>
			expect(view.getByText('VTube Studio authorization was removed.')).toBeTruthy()
		);
		expect(view.getByText('Not authorized')).toBeTruthy();
	});

	it('does not claim success if the cache does not confirm the operation', async () => {
		const onPresence = vi.fn().mockResolvedValue('empty');
		const view = render(
			VtubeConnectionView,
			props({ presence: onPresence, authorize: vi.fn().mockResolvedValue(undefined) })
		);
		await waitFor(() => expect(view.getByText('Not authorized')).toBeTruthy());
		await fireEvent.click(view.getByRole('button', { name: 'Authorize' }));
		await waitFor(() =>
			expect(view.getByRole('alert').textContent).toContain('could not be verified')
		);
		expect(view.queryByText('VTube Studio authorization is stored.')).toBeNull();
	});

	it('ignores stale presence and authorization results after session changes', async () => {
		let finishPresence!: (value: CredentialPresence) => void;
		const onPresence = vi
			.fn()
			.mockReturnValueOnce(
				new Promise<CredentialPresence>((resolve) => {
					finishPresence = resolve;
				})
			)
			.mockResolvedValueOnce('empty');
		const view = render(VtubeConnectionView, props({ presence: onPresence }));
		await view.rerender(
			props({ snapshot: { ...snapshot, session_id: 'session-b' }, presence: onPresence })
		);
		await waitFor(() => expect(view.getByText('Not authorized')).toBeTruthy());
		finishPresence('stored');
		await Promise.resolve();
		expect(view.getByText('Not authorized')).toBeTruthy();

		cleanup();
		let finishAuthorization!: () => void;
		const authorize = vi.fn(
			() =>
				new Promise<void>((resolve) => {
					finishAuthorization = resolve;
				})
		);
		const next = render(VtubeConnectionView, props({ authorize }));
		await waitFor(() => expect(next.getByText('Not authorized')).toBeTruthy());
		await fireEvent.click(next.getByRole('button', { name: 'Authorize' }));
		await next.rerender(props({ snapshot: { ...snapshot, session_id: 'session-c' }, authorize }));
		finishAuthorization();
		await Promise.resolve();
		expect(next.queryByText('VTube Studio authorization is stored.')).toBeNull();
	});

	it('ignores a presence result after unmount', async () => {
		let finish!: (value: CredentialPresence) => void;
		const view = render(
			VtubeConnectionView,
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
