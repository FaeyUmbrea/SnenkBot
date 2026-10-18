import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import EventsView from '../src/lib/EventsView.svelte';
import type { EventPage } from '../src/lib/contracts';

const page: EventPage = {
	events: [
		{
			id: 'record',
			source: 'twitch',
			name: 'channel.cheer',
			received_at_ms: 1000,
			replayable: true
		}
	],
	errors: []
};
beforeEach(() => {
	HTMLDialogElement.prototype.showModal = function () {
		this.setAttribute('open', '');
	};
	HTMLDialogElement.prototype.close = function () {
		this.removeAttribute('open');
	};
});
afterEach(cleanup);

describe('received events', () => {
	it('sends normal replay through ordinary dispatch and selected replay only to chosen recipients', async () => {
		const replay = vi.fn().mockResolvedValue({
			workflows_started: 1,
			consumers_delivered: ['overlay'],
			warnings: []
		});
		const view = render(EventsView, {
			load: async () => page,
			targets: async () => ({
				targets: [
					{ id: 'consumer:overlay', label: 'My overlay', kind: 'consumer' },
					{ id: 'workflow:alerts', label: 'Play alert', kind: 'workflow' }
				],
				errors: []
			}),
			replay
		});
		await waitFor(() => expect(view.getByRole('button', { name: /^Replay$/ })).toBeTruthy());
		await fireEvent.click(view.getByRole('button', { name: /^Replay$/ }));
		await waitFor(() => expect(replay).toHaveBeenCalledWith({ event_id: 'record', targets: null }));
		await fireEvent.click(view.getByRole('button', { name: 'Replay to…' }));
		await waitFor(() => expect(view.getByLabelText('My overlay')).toBeTruthy());
		expect(view.getByRole('button', { name: 'Replay selected' })).toHaveProperty('disabled', true);
		await fireEvent.click(view.getByLabelText('My overlay'));
		await fireEvent.click(view.getByRole('button', { name: 'Replay selected' }));
		await waitFor(() =>
			expect(replay).toHaveBeenLastCalledWith({ event_id: 'record', targets: ['consumer:overlay'] })
		);
		await fireEvent.click(view.getByRole('button', { name: 'Replay to…' }));
		await waitFor(() => expect(view.getByLabelText('Play alert')).toBeTruthy());
		await fireEvent.click(view.getByLabelText('Play alert'));
		await fireEvent.click(view.getByRole('button', { name: 'Replay selected' }));
		await waitFor(() =>
			expect(replay).toHaveBeenLastCalledWith({ event_id: 'record', targets: ['workflow:alerts'] })
		);
	});
	it('retains the selection after failure and does not send an empty selection', async () => {
		const replay = vi.fn().mockRejectedValue(new Error('private path'));
		const view = render(EventsView, {
			load: async () => page,
			targets: async () => ({
				targets: [{ id: 'consumer:overlay', label: 'My overlay', kind: 'consumer' }],
				errors: []
			}),
			replay
		});
		await waitFor(() => expect(view.getByRole('button', { name: 'Replay to…' })).toBeTruthy());
		await fireEvent.click(view.getByRole('button', { name: 'Replay to…' }));
		await waitFor(() => expect(view.getByLabelText('My overlay')).toBeTruthy());
		await fireEvent.click(view.getByLabelText('My overlay'));
		await fireEvent.click(view.getByRole('button', { name: 'Replay selected' }));
		await waitFor(() =>
			expect(view.getByRole('alert').textContent).toBe('The event could not be replayed.')
		);
		expect(view.getByLabelText('My overlay')).toHaveProperty('checked', true);
		expect(view.container.textContent).not.toContain('private path');
	});
});
