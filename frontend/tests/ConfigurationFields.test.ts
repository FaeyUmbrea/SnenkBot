import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import ConfigurationFields from '../src/lib/ConfigurationFields.svelte';
import StringListField from '../src/lib/StringListField.svelte';
import type { ConfigField } from '../src/lib/contracts/index';

afterEach(cleanup);

describe('shared string collection editor', () => {
	it('starts optional empty collections collapsed and adds one trimmed item with Enter', async () => {
		const onChange = vi.fn();
		const view = render(StringListField, {
			id: 'origins',
			label: 'Allowed origins',
			values: [],
			unique: true,
			collapsible: true,
			onChange
		});
		const details = view.getByText('Allowed origins').closest('details');
		expect(details?.open).toBe(false);
		await fireEvent.click(view.getByText('Allowed origins'));
		expect(details?.open).toBe(true);
		await fireEvent.input(view.getByLabelText('Add Allowed origins'), {
			target: { value: '  https://example.com  ' }
		});
		await fireEvent.keyDown(view.getByLabelText('Add Allowed origins'), { key: 'Enter' });
		expect(onChange).toHaveBeenCalledExactlyOnceWith(['https://example.com']);
	});

	it('allows duplicate list items but rejects duplicate set items', async () => {
		const listChange = vi.fn();
		const list = render(StringListField, {
			id: 'steps',
			label: 'Steps',
			values: ['repeat'],
			onChange: listChange
		});
		await fireEvent.input(list.getByLabelText('Add Steps'), { target: { value: 'repeat' } });
		await fireEvent.click(list.getByRole('button', { name: 'Add' }));
		expect(listChange).toHaveBeenCalledExactlyOnceWith(['repeat', 'repeat']);
		list.unmount();

		const setChange = vi.fn();
		const set = render(StringListField, {
			id: 'origins',
			label: 'Allowed origins',
			values: ['https://example.com'],
			unique: true,
			onChange: setChange
		});
		await fireEvent.input(set.getByLabelText('Add Allowed origins'), {
			target: { value: 'https://example.com' }
		});
		await fireEvent.click(set.getByRole('button', { name: 'Add' }));
		expect(setChange).not.toHaveBeenCalled();
		expect(set.getByRole('alert').textContent).toBe('This value is already listed.');
	});

	it('edits and removes existing rows without submitting the surrounding form', async () => {
		const onChange = vi.fn();
		const view = render(StringListField, {
			id: 'origins',
			label: 'Allowed origins',
			values: ['first', 'second'],
			unique: true,
			onChange
		});
		await fireEvent.change(view.getByLabelText('Allowed origins item 1'), {
			target: { value: 'changed' }
		});
		expect(onChange).toHaveBeenCalledWith(['changed', 'second']);
		await fireEvent.click(view.getByRole('button', { name: 'Remove Allowed origins item 2' }));
		expect(onChange).toHaveBeenCalledWith(['first']);
	});
});

describe('nested configuration choices', () => {
	it('collapses empty choice groups and retains unavailable saved values', async () => {
		const field: ConfigField = {
			id: 'permissions',
			label: 'Permissions',
			description: '',
			introduced_in: 1,
			required: true,
			choice_source: null,
			kind: {
				object: [
					{
						id: 'providers',
						label: 'Providers',
						description: '',
						introduced_in: 1,
						required: false,
						choice_source: { key: 'test.providers', depends_on: null },
						kind: 'string_set_map'
					}
				]
			}
		};
		const onChange = vi.fn();
		const view = render(ConfigurationFields, {
			fields: [field],
			values: {
				permissions: { providers: { lookup: ['retired-operation'], retired: ['saved-operation'] } }
			},
			choices: {
				'test.providers': {
					groups: [
						{
							id: 'lookup',
							label: 'Lookup',
							items: [{ value: 'resolve', label: 'Resolve icon', detail: 'Find game artwork.' }]
						},
						{
							id: 'other',
							label: 'Other',
							items: [{ value: 'query', label: 'Query data', detail: null }]
						}
					]
				}
			},
			onChange
		});
		const lookup = view.getByText('Lookup').closest('details');
		const other = view.getByText('Other').closest('details');
		expect(lookup?.open).toBe(true);
		expect(other?.open).toBe(false);
		expect(lookup?.textContent).toContain('1 selected');
		expect(lookup?.textContent).toContain('Saved choice unavailable: retired-operation');
		expect(view.getByText('Find game artwork.')).toBeTruthy();
		expect(view.container.querySelectorAll('fieldset')).toHaveLength(0);
		await fireEvent.click(view.getByRole('checkbox', { name: /Resolve icon/ }));
		expect(onChange).toHaveBeenCalledExactlyOnceWith({
			permissions: {
				providers: {
					lookup: ['retired-operation', 'resolve'],
					retired: ['saved-operation']
				}
			}
		});
	});
});

describe('shared directory control', () => {
	/** Describe a nested optional directory without integration-specific field names. */
	function fields(): ConfigField[] {
		return [
			{
				id: 'storage',
				label: 'Storage',
				description: '',
				introduced_in: 1,
				required: true,
				choice_source: null,
				kind: {
					object: [
						{
							id: 'folder',
							label: 'Folder',
							description: '',
							introduced_in: 1,
							required: false,
							choice_source: null,
							kind: 'directory'
						}
					]
				}
			}
		];
	}

	it('passes nested paths to the picker and preserves unrelated draft values', async () => {
		const pickDirectory = vi.fn().mockResolvedValue('/new/folder');
		const onChange = vi.fn();
		const view = render(ConfigurationFields, {
			fields: fields(),
			values: { storage: { folder: '/old/folder', sibling: 42 }, untouched: true },
			pickDirectory,
			onChange
		});
		await fireEvent.click(view.getByRole('button', { name: 'Browse…' }));
		await waitFor(() =>
			expect(onChange).toHaveBeenCalledExactlyOnceWith({
				storage: { folder: '/new/folder', sibling: 42 },
				untouched: true
			})
		);
		expect(pickDirectory).toHaveBeenCalledExactlyOnceWith(['storage', 'folder'], '/old/folder');
	});

	it('keeps cancellation unchanged and allows clearing optional paths manually', async () => {
		const pickDirectory = vi.fn().mockResolvedValue(null);
		const onChange = vi.fn();
		const view = render(ConfigurationFields, {
			fields: fields(),
			values: { storage: { folder: '/old/folder' } },
			pickDirectory,
			onChange
		});
		await fireEvent.click(view.getByRole('button', { name: 'Browse…' }));
		await waitFor(() =>
			expect(view.getByRole('button', { name: 'Browse…' })).toHaveProperty('disabled', false)
		);
		expect(onChange).not.toHaveBeenCalled();
		await fireEvent.input(view.getByLabelText('Folder'), { target: { value: '' } });
		expect(onChange).toHaveBeenCalledExactlyOnceWith({ storage: { folder: null } });
	});
});
