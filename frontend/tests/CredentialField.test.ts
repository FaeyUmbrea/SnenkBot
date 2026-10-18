import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import CredentialField from '../src/lib/CredentialField.svelte';
import type { CredentialDescriptor } from '../src/lib/credential';

const descriptor: CredentialDescriptor = {
	id: 'test-secret',
	label: 'Test secret',
	emptyDescription: 'No secret stored.',
	storedDescription: 'A secret is stored.',
	unavailableDescription: 'Secret status is unavailable.',
	saveLabel: 'Save secret',
	savedMessage: 'Secret saved.',
	saveError: 'The secret could not be saved.',
	removeLabel: 'Remove secret',
	removedMessage: 'Secret removed.'
};

afterEach(cleanup);

describe('CredentialField', () => {
	it('keeps a newer draft while a save is pending and rejects stale status reads', async () => {
		let finishSave!: (value: 'stored') => void;
		let finishStatus!: (value: 'empty') => void;
		const save = new Promise<'stored'>((resolve) => (finishSave = resolve));
		const status = new Promise<'empty'>((resolve) => (finishStatus = resolve));
		const onSave = vi.fn().mockReturnValue(save);
		const view = render(CredentialField, {
			descriptor,
			contextKey: 'first',
			onSave,
			onRefresh: vi.fn().mockReturnValue(status)
		});
		await fireEvent.change(view.getByLabelText('Test secret'), { target: { value: 'first' } });
		await fireEvent.click(view.getByRole('button', { name: 'Save secret' }));
		await fireEvent.change(view.getByLabelText('Test secret'), { target: { value: 'newer' } });
		finishSave('stored');
		await waitFor(() => expect(view.getByText('A secret is stored.')).toBeTruthy());
		finishStatus('empty');
		await Promise.resolve();
		expect(view.getByLabelText('Test secret')).toHaveProperty('value', 'newer');
		expect(view.getByText('A secret is stored.')).toBeTruthy();
		expect(onSave).toHaveBeenCalledExactlyOnceWith('first');
	});

	it('resets for a new context and ignores the previous save result', async () => {
		let finish!: (value: 'stored') => void;
		const pending = new Promise<'stored'>((resolve) => (finish = resolve));
		const oldSave = vi.fn().mockReturnValue(pending);
		const newSave = vi.fn().mockResolvedValue('stored');
		const view = render(CredentialField, {
			descriptor,
			contextKey: 'first',
			onSave: oldSave
		});
		await fireEvent.change(view.getByLabelText('Test secret'), { target: { value: 'old' } });
		await fireEvent.click(view.getByRole('button', { name: 'Save secret' }));
		await view.rerender({ descriptor, contextKey: 'second', onSave: newSave });
		await waitFor(() => expect(view.getByLabelText('Test secret')).toHaveProperty('value', ''));
		finish('stored');
		await Promise.resolve();
		expect(view.queryByText('Secret saved.')).toBeNull();
		expect(view.getByText('Secret status is unavailable.')).toBeTruthy();
		await fireEvent.change(view.getByLabelText('Test secret'), { target: { value: 'new' } });
		await fireEvent.click(view.getByRole('button', { name: 'Save secret' }));
		await waitFor(() => expect(newSave).toHaveBeenCalledExactlyOnceWith('new'));
	});

	it('confirms removal and keeps the draft when storage fails', async () => {
		const onSave = vi
			.fn()
			.mockRejectedValueOnce(new Error('backend secret'))
			.mockResolvedValue('empty');
		const view = render(CredentialField, {
			descriptor,
			contextKey: 'first',
			presence: 'stored',
			onSave
		});
		await fireEvent.change(view.getByLabelText('Test secret'), { target: { value: 'keep this' } });
		await fireEvent.click(view.getByRole('button', { name: 'Save secret' }));
		await waitFor(() =>
			expect(view.getByRole('alert')).toHaveProperty('textContent', descriptor.saveError)
		);
		expect(view.container.textContent).not.toContain('backend secret');
		expect(view.getByLabelText('Test secret')).toHaveProperty('value', 'keep this');
		await fireEvent.click(view.getByRole('button', { name: 'Remove secret' }));
		await waitFor(() => expect(view.getByText('Secret removed.')).toBeTruthy());
		expect(onSave).toHaveBeenLastCalledWith(null);
		expect(view.getByRole('button', { name: 'Remove secret' })).toHaveProperty('disabled', true);
	});
});
