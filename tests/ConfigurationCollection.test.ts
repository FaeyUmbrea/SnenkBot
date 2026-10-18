import { cleanup, fireEvent, render, waitFor, within } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import ConfigurationCollection, {
	type CollectionSnapshot
} from '../src/lib/ConfigurationCollection.svelte';
import { DesktopRequestError } from '../src/lib/desktop';

afterEach(cleanup);

/** Supply unrelated settings to detect API-specific behavior in the shared editor. */
function fixture(): CollectionSnapshot {
	return {
		definition: {
			schema: {
				id: 'test.devices',
				title: 'Devices',
				version: 1,
				outputs: [],
				fields: [
					{
						id: 'address',
						label: 'Address',
						description: '',
						introduced_in: 1,
						kind: 'text',
						required: true,
						choice_source: null
					}
				]
			},
			defaults: { address: '' }
		},
		records: [{ id: 'device', title: 'My device', values: { address: 'old' } }],
		actions: [],
		choices: {}
	};
}

describe('shared configuration collection', () => {
	it('runs declared actions only in an existing editor and retains its unsaved draft', async () => {
		const snapshot = fixture();
		snapshot.actions = [
			{
				id: 'rotate',
				label: 'Replace access',
				description: 'Replace this entry’s access credential.',
				confirmation: 'Old access stops immediately.',
				success_message: 'Access replaced.'
			}
		];
		const act = vi.fn().mockResolvedValue('new-private-credential');
		const close = vi.fn().mockResolvedValue(undefined);
		const view = render(ConfigurationCollection, {
			title: 'Devices',
			load: async () => snapshot,
			save: async () => {},
			remove: async () => {},
			act,
			close
		});
		await waitFor(() => expect(view.getByRole('button', { name: 'Edit' })).toBeTruthy());
		expect(view.queryByRole('button', { name: 'Replace access' })).toBeNull();
		await fireEvent.click(view.getByRole('button', { name: 'Edit' }));
		await fireEvent.input(view.getByLabelText('Address'), { target: { value: 'unsaved' } });
		await fireEvent.click(view.getByRole('button', { name: 'Replace access' }));
		expect(act).not.toHaveBeenCalled();
		const confirmation = view.getByRole('dialog', { name: 'Replace access' });
		expect(confirmation.textContent).toContain('Old access stops immediately.');
		await fireEvent.click(within(confirmation).getByRole('button', { name: 'Replace access' }));
		await waitFor(() =>
			expect(view.getByLabelText('New credential')).toHaveProperty(
				'value',
				'new-private-credential'
			)
		);
		expect(act).toHaveBeenCalledExactlyOnceWith('device', 'rotate');
		expect(view.getByLabelText('Address')).toHaveProperty('value', 'unsaved');
		expect(close).not.toHaveBeenCalled();
		expect(view.getByRole('status').textContent).toBe('Access replaced.');
		await fireEvent.click(view.getByRole('button', { name: 'Hide credential' }));
		expect(view.getByLabelText('Address')).toHaveProperty('value', 'unsaved');
	});

	it('cancels confirmation and reports copy action failures without discarding edits', async () => {
		const snapshot = fixture();
		snapshot.actions = [
			{
				id: 'rotate',
				label: 'Replace access',
				description: '',
				confirmation: 'Replace old access?',
				success_message: 'Done.'
			},
			{
				id: 'copy',
				label: 'Copy address',
				description: '',
				confirmation: null,
				success_message: 'Address copied.'
			}
		];
		const act = vi.fn().mockRejectedValue(
			new DesktopRequestError({
				code: 'invalid_values',
				message: 'The address is unavailable. Configure a folder first.'
			})
		);
		const view = render(ConfigurationCollection, {
			title: 'Devices',
			load: async () => snapshot,
			save: async () => {},
			remove: async () => {},
			act
		});
		await waitFor(() => expect(view.getByRole('button', { name: 'Edit' })).toBeTruthy());
		await fireEvent.click(view.getByRole('button', { name: 'Edit' }));
		await fireEvent.input(view.getByLabelText('Address'), { target: { value: 'draft' } });
		await fireEvent.click(view.getByRole('button', { name: 'Replace access' }));
		await fireEvent.click(view.getByRole('button', { name: 'Cancel action' }));
		expect(act).not.toHaveBeenCalled();
		await fireEvent.click(view.getByRole('button', { name: 'Copy address' }));
		await waitFor(() =>
			expect(view.getByRole('alert').textContent).toContain('Configure a folder first.')
		);
		expect(view.getByLabelText('Address')).toHaveProperty('value', 'draft');
		act.mockResolvedValueOnce(undefined);
		await fireEvent.click(view.getByRole('button', { name: 'Copy address' }));
		await waitFor(() => expect(view.getByRole('status').textContent).toBe('Address copied.'));
		expect(view.queryByLabelText('New credential')).toBeNull();
		expect(view.getByLabelText('Address')).toHaveProperty('value', 'draft');
	});

	it('opens the native baseline and retains a conflicting edit', async () => {
		const open = vi.fn().mockResolvedValue({ address: 'current' });
		const close = vi.fn().mockResolvedValue(undefined);
		const save = vi
			.fn()
			.mockRejectedValue(
				new DesktopRequestError({ code: 'save_conflict', message: 'Changed elsewhere.' })
			);
		const view = render(ConfigurationCollection, {
			title: 'Devices',
			load: async () => fixture(),
			save,
			remove: async () => {},
			open,
			close
		});
		await waitFor(() => expect(view.getByRole('button', { name: 'Edit' })).toBeTruthy());
		await fireEvent.click(view.getByRole('button', { name: 'Edit' }));
		await waitFor(() => expect(view.getByLabelText('Address')).toHaveProperty('value', 'current'));
		await fireEvent.input(view.getByLabelText('Address'), { target: { value: 'draft' } });
		await fireEvent.click(view.getByRole('button', { name: 'Save changes' }));
		await waitFor(() => expect(view.getByRole('alert').textContent).toBe('Changed elsewhere.'));
		expect(view.getByLabelText('Address')).toHaveProperty('value', 'draft');
		expect(save).toHaveBeenCalledWith('device', { address: 'draft' });
		expect(close).not.toHaveBeenCalled();
		await fireEvent.click(view.getByRole('button', { name: 'Cancel' }));
		await waitFor(() => expect(close).toHaveBeenCalledOnce());
	});
	it('preserves a newly issued credential when refreshing records fails', async () => {
		const load = vi
			.fn()
			.mockResolvedValueOnce(fixture())
			.mockRejectedValue(new Error('private path'));
		const view = render(ConfigurationCollection, {
			title: 'Devices',
			load,
			save: async () => 'new-secret',
			remove: async () => {}
		});
		await waitFor(() => expect(view.getByRole('button', { name: 'Add entry' })).toBeTruthy());
		await fireEvent.click(view.getByRole('button', { name: 'Add entry' }));
		await waitFor(() => expect(view.getByLabelText('Address')).toBeTruthy());
		await fireEvent.input(view.getByLabelText('Address'), { target: { value: 'new' } });
		await fireEvent.click(view.getByRole('button', { name: 'Create entry' }));
		await waitFor(() =>
			expect(view.getByLabelText('New credential')).toHaveProperty('value', 'new-secret')
		);
		await waitFor(() =>
			expect(view.getByRole('alert').textContent).toBe('Settings could not be loaded.')
		);
		expect(view.container.textContent).not.toContain('private path');
	});
});
