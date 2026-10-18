import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import EventTestDialog from '../src/lib/EventTestDialog.svelte';
import type { EventTestOptions } from '../src/lib/contracts';

/** A future sender proves the dialog derives its fields and target identity from registration. */
const options: EventTestOptions = {
	consumers: [{ id: 'overlay', name: 'Stream overlay', channels: ['future.channel'] }],
	senders: [
		{
			channel: 'future.channel',
			target_field: 'recipients',
			definition: {
				schema: {
					id: 'future.sender',
					version: 3,
					title: 'Future sample',
					outputs: [],
					fields: [
						{
							id: 'recipients',
							label: 'Internal recipients',
							description: '',
							introduced_in: 1,
							kind: 'string_set',
							required: true,
							choice_source: null
						},
						{
							id: 'caption',
							label: 'Caption',
							description: '',
							introduced_in: 1,
							kind: 'text',
							required: true,
							choice_source: null
						}
					]
				},
				defaults: { recipients: [], caption: 'Generated default' }
			}
		}
	]
};

beforeEach(() => {
	HTMLDialogElement.prototype.showModal = function () {
		this.open = true;
	};
	HTMLDialogElement.prototype.close = function () {
		this.open = false;
	};
});
afterEach(cleanup);

describe('direct overlay testing', () => {
	it('uses registered defaults and fields and keeps the draft ready for repeated sends', async () => {
		const send = vi.fn().mockResolvedValue(undefined);
		const view = render(EventTestDialog, {
			open: true,
			load: async () => options,
			choices: vi.fn(),
			send,
			onClose: vi.fn()
		});
		await waitFor(() =>
			expect(view.getByLabelText('Caption')).toHaveProperty('value', 'Generated default')
		);
		expect(view.queryByLabelText('Internal recipients')).toBeNull();
		await fireEvent.input(view.getByLabelText('Caption'), { target: { value: 'My sample' } });
		await fireEvent.click(view.getByRole('button', { name: /^Send$/ }));
		await waitFor(() => expect(view.getByRole('status').textContent).toContain('Stream overlay'));
		expect(send).toHaveBeenCalledWith({
			action_id: 'future.sender',
			version: 3,
			consumer_id: 'overlay',
			values: { recipients: [], caption: 'My sample' }
		});
		await fireEvent.click(view.getByRole('button', { name: /^Send$/ }));
		await waitFor(() => expect(send).toHaveBeenCalledTimes(2));
		expect(view.getByLabelText('Caption')).toHaveProperty('value', 'My sample');
	});

	it('offers only senders allowed by the selected consumer grants', async () => {
		const send = vi.fn();
		const view = render(EventTestDialog, {
			open: true,
			load: async () => ({
				...options,
				consumers: [{ id: 'other', name: 'Other overlay', channels: [] }]
			}),
			choices: vi.fn(),
			send,
			onClose: vi.fn()
		});
		await waitFor(() =>
			expect(
				view.getByText('This consumer has no channels with available test senders.')
			).toBeTruthy()
		);
		expect(view.getByRole('button', { name: /^Send$/ })).toHaveProperty('disabled', true);
		expect(send).not.toHaveBeenCalled();
	});
});

it('loads generated choice sources by their declared key and retains native errors without losing the draft', async () => {
	const field = {
		id: 'options',
		label: 'Options',
		description: '',
		introduced_in: 1,
		required: true,
		kind: 'string_set' as const,
		choice_source: { key: 'future.options', depends_on: null }
	};
	const generated = {
		...options,
		senders: [
			{
				...options.senders[0],
				definition: {
					...options.senders[0].definition,
					schema: {
						...options.senders[0].definition.schema,
						fields: [...options.senders[0].definition.schema.fields, field]
					},
					defaults: { ...options.senders[0].definition.defaults, options: [] }
				}
			}
		]
	};
	const choose = vi.fn(async () => [{ value: 'one', label: 'First option', detail: null }]);
	const send = vi.fn().mockRejectedValue(new Error('private transport details'));
	const view = render(EventTestDialog, {
		open: true,
		load: async () => generated,
		choices: choose,
		send,
		onClose: vi.fn()
	});
	await waitFor(() => expect(view.getByText('Options')).toBeTruthy());
	await fireEvent.click(view.getByText('Options'));
	await fireEvent.click(await view.findByRole('checkbox', { name: 'First option' }));
	await fireEvent.click(view.getByRole('button', { name: /^Send$/ }));
	await waitFor(() => expect(view.getByRole('alert').textContent).toContain('could not be sent'));
	expect(view.getByLabelText('Caption')).toHaveProperty('value', 'Generated default');
	expect(view.container.textContent).not.toContain('private transport details');
	expect(send).toHaveBeenCalledWith(
		expect.objectContaining({ values: expect.objectContaining({ options: ['one'] }) })
	);
	expect(choose).toHaveBeenCalledWith({
		action_id: 'future.sender',
		field_id: 'options',
		depends_on: null
	});
});
