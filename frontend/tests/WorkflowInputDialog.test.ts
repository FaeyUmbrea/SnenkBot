import { fireEvent, render, waitFor } from '@testing-library/svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import WorkflowInputDialog from '../src/lib/WorkflowInputDialog.svelte';
import type { InputRequest } from '../src/lib/contracts/index';

const request: InputRequest = {
	request_id: 'request-a',
	title: 'Start the broadcast',
	fields: [
		{ id: 'name', label: 'Name', default: null, required: true },
		{ id: 'count', label: 'Count', default: null, required: false },
		{ id: 'enabled', label: 'Enabled', default: null, required: false },
		{ id: 'internal_id', default: null, required: false },
		{ id: 'empty', label: 'Empty', default: null, required: false }
	],
	defaults: { name: 'Default name', count: 12, enabled: true, internal_id: { option: 1 } },
	values: { name: '', empty: null },
	validating: false,
	error: null
};

function deferred() {
	let resolve!: () => void;
	let reject!: (reason: unknown) => void;
	const promise = new Promise<void>((finish, fail) => {
		resolve = finish;
		reject = fail;
	});
	return { promise, resolve, reject };
}

beforeEach(() => {
	Object.defineProperty(HTMLDialogElement.prototype, 'showModal', {
		configurable: true,
		value: vi.fn(function (this: HTMLDialogElement) {
			this.open = true;
		})
	});
	Object.defineProperty(HTMLDialogElement.prototype, 'close', {
		configurable: true,
		value: vi.fn(function (this: HTMLDialogElement) {
			this.open = false;
		})
	});
});

describe('WorkflowInputDialog', () => {
	it('closes its native modal on unmount without cancelling the backend request', async () => {
		const onCancel = vi.fn();
		const view = render(WorkflowInputDialog, { request, onSubmit: vi.fn(), onCancel });
		const dialog = view.getByRole('dialog');
		expect(dialog).toHaveProperty('open', true);
		await view.unmount();
		expect(dialog).toHaveProperty('open', false);
		expect(HTMLDialogElement.prototype.close).toHaveBeenCalledOnce();
		expect(onCancel).not.toHaveBeenCalled();
	});

	it('uses resolved values before defaults and submits every field as a string', async () => {
		const onSubmit = vi.fn().mockResolvedValue(undefined);
		const view = render(WorkflowInputDialog, { request, onSubmit, onCancel: vi.fn() });
		expect(view.getByLabelText(/Name/)).toHaveProperty('value', '');
		expect(view.getByLabelText('Count')).toHaveProperty('value', '12');
		expect(view.getByLabelText('Enabled')).toHaveProperty('type', 'text');
		expect(view.getByLabelText('Input 4')).toHaveProperty('value', '{"option":1}');
		expect(view.getByLabelText('Empty')).toHaveProperty('value', '');
		expect(view.queryByText('internal_id')).toBeNull();
		await fireEvent.click(view.getByRole('button', { name: 'Continue' }));
		expect(onSubmit).toHaveBeenCalledWith({
			request_id: 'request-a',
			values: { name: '', count: '12', enabled: 'true', internal_id: '{"option":1}', empty: '' }
		});
		expect(view.getByRole('dialog')).toHaveProperty('open', true);
		expect(view.getByRole('button', { name: 'Continue' })).toHaveProperty('disabled', true);
	});

	it('preserves edits through backend validation and rejection and permits retry', async () => {
		const pending = deferred();
		const onSubmit = vi.fn().mockReturnValueOnce(pending.promise).mockResolvedValue(undefined);
		const onCancel = vi.fn();
		const view = render(WorkflowInputDialog, { request, onSubmit, onCancel });
		await fireEvent.input(view.getByLabelText(/Name/), { target: { value: 'Submitted edit' } });
		await fireEvent.click(view.getByRole('button', { name: 'Continue' }));
		await fireEvent.click(view.getByRole('button', { name: 'Continue' }));
		expect(onSubmit).toHaveBeenCalledOnce();
		await view.rerender({
			request: { ...request, validating: true, values: { name: 'Submitted edit' } },
			onSubmit,
			onCancel
		});
		await fireEvent.input(view.getByLabelText(/Name/), { target: { value: 'Latest edit' } });
		pending.resolve();
		await view.rerender({
			request: {
				...request,
				values: { name: 'Submitted edit' },
				error: 'Choose a broadcast name.'
			},
			onSubmit,
			onCancel
		});
		await waitFor(() =>
			expect(view.getByRole('button', { name: 'Continue' })).toHaveProperty('disabled', false)
		);
		expect(view.getByLabelText(/Name/)).toHaveProperty('value', 'Latest edit');
		expect(view.getByRole('alert').textContent).toBe('Choose a broadcast name.');
		await fireEvent.click(view.getByRole('button', { name: 'Continue' }));
		expect(onSubmit).toHaveBeenLastCalledWith(
			expect.objectContaining({ values: expect.objectContaining({ name: 'Latest edit' }) })
		);
	});

	it('retains the draft and safe feedback when submission fails', async () => {
		const onSubmit = vi.fn().mockRejectedValue(new Error('internal secret detail'));
		const view = render(WorkflowInputDialog, { request, onSubmit, onCancel: vi.fn() });
		await fireEvent.input(view.getByLabelText(/Name/), { target: { value: 'Keep this' } });
		await fireEvent.click(view.getByRole('button', { name: 'Continue' }));
		await waitFor(() =>
			expect(view.getByRole('alert').textContent).toBe('Could not submit your input. Try again.')
		);
		expect(view.getByLabelText(/Name/)).toHaveProperty('value', 'Keep this');
		expect(view.getByRole('dialog')).toHaveProperty('open', true);
		await fireEvent.click(view.getByRole('button', { name: 'Continue' }));
		await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(2));
	});

	it.each(['resolve', 'reject'] as const)(
		'ignores an old submission %s after replacing the request',
		async (outcome) => {
			const old = deferred();
			const current = deferred();
			const onSubmit = vi.fn().mockReturnValueOnce(old.promise).mockReturnValue(current.promise);
			const onCancel = vi.fn();
			const view = render(WorkflowInputDialog, { request, onSubmit, onCancel });
			await fireEvent.input(view.getByLabelText(/Name/), { target: { value: 'Old draft' } });
			await fireEvent.click(view.getByRole('button', { name: 'Continue' }));
			await view.rerender({
				request: { ...request, request_id: 'request-b', values: { name: 'New request' } },
				onSubmit,
				onCancel
			});
			expect(view.getByLabelText(/Name/)).toHaveProperty('value', 'New request');
			await fireEvent.click(view.getByRole('button', { name: 'Continue' }));
			if (outcome === 'reject') old.reject(new Error('old failure'));
			else old.resolve();
			await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(2));
			expect(view.getByRole('button', { name: 'Continue' })).toHaveProperty('disabled', true);
			expect(view.queryByRole('alert')).toBeNull();
			expect(view.getByLabelText(/Name/)).toHaveProperty('value', 'New request');
			current.resolve();
		}
	);

	it('routes Escape to cancellation during validation and waits for backend closure', async () => {
		const pending = deferred();
		const onCancel = vi.fn().mockReturnValue(pending.promise);
		const onSubmit = vi.fn();
		const view = render(WorkflowInputDialog, {
			request: { ...request, validating: true },
			onSubmit,
			onCancel
		});
		const dialog = view.getByRole('dialog');
		const event = new Event('cancel', { cancelable: true });
		dialog.dispatchEvent(event);
		await fireEvent.click(view.getByRole('button', { name: 'Cancel' }));
		expect(event.defaultPrevented).toBe(true);
		expect(onCancel).toHaveBeenCalledExactlyOnceWith({ request_id: 'request-a' });
		expect(dialog).toHaveProperty('open', true);
		pending.resolve();
		await waitFor(() =>
			expect(view.getByRole('button', { name: 'Cancel' })).toHaveProperty('disabled', true)
		);
		await view.rerender({ request: null, onSubmit, onCancel });
		expect(dialog).toHaveProperty('open', false);
		expect(HTMLDialogElement.prototype.close).toHaveBeenCalledOnce();
	});

	it('retains edits after failed cancellation and ignores a late failure for another request', async () => {
		const old = deferred();
		const onCancel = vi
			.fn()
			.mockRejectedValueOnce(new Error('internal detail'))
			.mockReturnValue(old.promise);
		const onSubmit = vi.fn();
		const view = render(WorkflowInputDialog, { request, onSubmit, onCancel });
		await fireEvent.input(view.getByLabelText(/Name/), { target: { value: 'Keep draft' } });
		await fireEvent.click(view.getByRole('button', { name: 'Cancel' }));
		await waitFor(() =>
			expect(view.getByRole('alert').textContent).toBe('Could not cancel the run. Try again.')
		);
		expect(view.getByLabelText(/Name/)).toHaveProperty('value', 'Keep draft');
		await fireEvent.click(view.getByRole('button', { name: 'Cancel' }));
		await view.rerender({
			request: { ...request, request_id: 'request-b', values: { name: 'Replacement' } },
			onSubmit,
			onCancel
		});
		old.reject(new Error('late failure'));
		await waitFor(() => expect(view.getByLabelText(/Name/)).toHaveProperty('value', 'Replacement'));
		expect(view.queryByRole('alert')).toBeNull();
		expect(view.getByRole('button', { name: 'Cancel' })).toHaveProperty('disabled', false);
	});
});
