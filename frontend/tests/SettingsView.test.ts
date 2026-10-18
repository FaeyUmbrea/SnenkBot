import { fireEvent, render, waitFor } from '@testing-library/svelte';
import { expect, it, vi } from 'vitest';
import SettingsView from '../src/lib/SettingsView.svelte';
import { createDesktopClient } from '../src/lib/desktop';
import type { ConfigurationSnapshot } from '../src/lib/contracts';

const snapshot: ConfigurationSnapshot = {
	session_id: 'synthetic-settings',
	revision: 0,
	module: 'emotes',
	schema: {
		id: 'example.connection',
		version: 1,
		title: 'Example connector',
		outputs: [],
		fields: [
			{
				id: 'ready',
				label: 'Enabled',
				description: '',
				introduced_in: 1,
				kind: 'toggle',
				required: false,
				choice_source: null
			}
		]
	},
	defaults: { ready: false },
	values: { ready: true },
	credential: null,
	authorization: {
		id: 'example-pairing',
		label: 'Example pairing',
		description: 'Approve this connection in the companion application.',
		prerequisite_field: 'ready',
		prerequisite_message: 'Save the connection before pairing.'
	},
	authorization_ready: false,
	collection: {
		id: 'example.cues',
		title: 'Cue list',
		item_label: 'cue',
		description: 'Manage named cues.'
	}
};

it('renders pairing and collection names from a synthetic native settings snapshot', async () => {
	const transport = vi.fn(async (command: string) => {
		if (command === 'configuration_authorization_status') return 'empty';
		if (command === 'configuration_collection_load')
			return {
				definition: {
					schema: {
						id: 'example.cue',
						version: 1,
						title: 'Cue',
						outputs: [],
						fields: [
							{
								id: 'name',
								label: 'Cue name',
								description: '',
								introduced_in: 1,
								kind: 'text',
								required: true,
								choice_source: null
							}
						]
					},
					defaults: { name: '' }
				},
				records: [],
				choices: {}
			};
		return undefined;
	});
	const view = render(SettingsView, {
		snapshot,
		client: createDesktopClient(transport),
		onSave: vi.fn()
	});
	await waitFor(() => expect(view.getByText('Example pairing')).toBeTruthy());
	expect(view.getByText('Approve this connection in the companion application.')).toBeTruthy();
	expect(view.getByText('Save the connection before pairing.')).toBeTruthy();
	expect(view.getByRole('button', { name: 'Authorize' })).toHaveProperty('disabled', true);
	expect(transport).toHaveBeenCalledWith('configuration_authorization_status', {
		request: { session_id: 'synthetic-settings', authorization_id: 'example-pairing' }
	});
	expect(transport).not.toHaveBeenCalledWith('configuration_collection_load', expect.anything());
	await fireEvent.click(view.getByRole('button', { name: 'Cue list' }));
	await waitFor(() => expect(view.getByRole('button', { name: 'Add cue' })).toBeTruthy());
	expect(view.getByText('Manage named cues.')).toBeTruthy();
	expect(view.getByText('No cue list yet. Add one to get started.')).toBeTruthy();
	expect(transport).toHaveBeenCalledWith('configuration_collection_load', {
		request: { session_id: 'synthetic-settings', collection_id: 'example.cues' }
	});
	expect(
		transport.mock.calls.filter(([command]) => command === 'configuration_collection_load')
	).toHaveLength(1);
	await fireEvent.click(view.getByRole('button', { name: 'Add cue' }));
	await fireEvent.input(view.getByLabelText('Cue name'), { target: { value: 'Draft cue' } });
	await fireEvent.click(view.getByRole('button', { name: 'Example connector' }));
	await fireEvent.click(view.getByRole('button', { name: 'Cue list' }));
	expect(view.getByDisplayValue('Draft cue')).toBeTruthy();
	expect(
		transport.mock.calls.filter(([command]) => command === 'configuration_collection_load')
	).toHaveLength(1);
});
