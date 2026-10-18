import { fireEvent, render, waitFor } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import WorkflowLibraryView from '../src/lib/WorkflowLibraryView.svelte';
import { DesktopRequestError } from '../src/lib/desktop';
import type { WorkflowStatus } from '../src/lib/contracts/index';

function workflow(id: string, overrides: Partial<WorkflowStatus> = {}): WorkflowStatus {
	return {
		id,
		title: `Workflow ${id}`,
		enabled: true,
		trigger_summary: 'Manual run',
		step_count: 1,
		category: 'Automation',
		capabilities: [],
		capability_titles: {},
		integration_usage: {},
		revision: 1,
		has_steps: true,
		error: null,
		...overrides
	};
}

function props(workflows: readonly WorkflowStatus[] = [workflow('one'), workflow('two')]) {
	return {
		workflows,
		onOpen: vi.fn(),
		onRun: vi.fn().mockResolvedValue(undefined),
		onCreate: vi.fn().mockResolvedValue(undefined)
	};
}

function deferred<T = void>() {
	let resolve!: (value: T) => void;
	let reject!: (reason: unknown) => void;
	const promise = new Promise<T>((success, failure) => {
		resolve = success;
		reject = failure;
	});
	return { promise, resolve, reject };
}

describe('WorkflowLibraryView', () => {
	it('restores an externally changed search on the first page and searches trigger labels', async () => {
		const options = props(Array.from({ length: 51 }, (_, index) => workflow(String(index))));
		const view = render(WorkflowLibraryView, { ...options, query: '' });
		await fireEvent.click(view.getByRole('button', { name: 'Next page' }));
		await view.rerender({ ...options, query: 'Manual run' });
		expect(view.getByText('Page 1 of 2')).toBeTruthy();
		expect(view.getByRole('button', { name: 'Workflow 0' })).toBeTruthy();
	});
	it('searches by title and resets paging, while keeping the query controlled by its parent', async () => {
		const options = props(Array.from({ length: 51 }, (_, index) => workflow(String(index))));
		const onQueryChange = vi.fn();
		const view = render(WorkflowLibraryView, { ...options, query: '', onQueryChange });
		await fireEvent.click(view.getByRole('button', { name: 'Next page' }));
		expect(view.getByText('Page 2 of 2')).toBeTruthy();
		const search = view.getByRole('textbox', { name: 'Search workflows' });
		await fireEvent.input(search, { target: { value: 'Workflow 50' } });
		expect(onQueryChange).toHaveBeenCalledWith('Workflow 50');
		await view.rerender({ ...options, query: 'Workflow 50', onQueryChange });
		expect(view.queryByRole('navigation', { name: 'Workflow pages' })).toBeNull();
		expect(view.getByRole('button', { name: 'Workflow 50' })).toBeTruthy();
		expect(view.queryByRole('button', { name: 'Workflow 2' })).toBeNull();
	});

	it('clamps the current page when workflows are removed', async () => {
		const all = Array.from({ length: 51 }, (_, index) => workflow(String(index)));
		const options = props(all);
		const view = render(WorkflowLibraryView, options);
		await fireEvent.click(view.getByRole('button', { name: 'Next page' }));
		await view.rerender({ ...options, workflows: all.slice(0, 10) });
		expect(view.queryByText('Page 2 of 1')).toBeNull();
		expect(view.getByRole('button', { name: 'Workflow 9' })).toBeTruthy();
	});

	it('opens disabled and unavailable workflows, and only runs enabled runnable workflows', async () => {
		const options = props([
			workflow('ready'),
			workflow('disabled', { enabled: false }),
			workflow('broken', { error: 'Malformed workflow' }),
			workflow('empty', { has_steps: false })
		]);
		const view = render(WorkflowLibraryView, options);
		expect(view.getAllByText('Enabled')).toHaveLength(2);
		expect(view.getByText('Disabled')).toBeTruthy();
		expect(view.getByText('Unavailable')).toBeTruthy();
		await fireEvent.click(view.getByRole('button', { name: 'Workflow disabled' }));
		await fireEvent.click(view.getByRole('button', { name: 'Workflow broken' }));
		expect(options.onOpen.mock.calls).toEqual([['disabled'], ['broken']]);
		expect(view.getByRole('button', { name: 'Run Workflow ready' })).toBeTruthy();
		expect(view.queryByRole('button', { name: 'Run Workflow empty' })).toBeNull();
		await fireEvent.click(view.getByRole('button', { name: 'Run Workflow ready' }));
		expect(options.onRun).toHaveBeenCalledExactlyOnceWith('ready');
	});

	it('guards repeat runs, shows safe errors, and lets the user dismiss feedback', async () => {
		const pending = deferred();
		const options = props([workflow('one')]);
		options.onRun.mockReturnValue(pending.promise);
		const view = render(WorkflowLibraryView, options);
		const run = view.getByRole('button', { name: 'Run Workflow one' });
		await fireEvent.click(run);
		await fireEvent.click(run);
		expect(options.onRun).toHaveBeenCalledOnce();
		pending.reject(new Error('private internal detail'));
		await waitFor(() => expect(view.getByRole('alert').textContent).toContain('could not be run'));
		expect(view.queryByText('private internal detail')).toBeNull();
		await fireEvent.click(view.getByRole('button', { name: 'Dismiss run error for Workflow one' }));
		expect(view.queryByRole('alert')).toBeNull();
	});

	it('removes existing run feedback when a newer workflow revision arrives', async () => {
		const options = props([workflow('one')]);
		options.onRun.mockRejectedValue(new Error('private failure'));
		const view = render(WorkflowLibraryView, options);
		await fireEvent.click(view.getByRole('button', { name: 'Run Workflow one' }));
		await waitFor(() => expect(view.getByRole('alert')).toBeTruthy());
		await view.rerender({ ...options, workflows: [workflow('one', { revision: 2 })] });
		await waitFor(() => expect(view.queryByRole('alert')).toBeNull());
	});

	it('gates manual runs when the view is disabled and ignores stale run failures', async () => {
		const pending = deferred();
		const options = props([workflow('one')]);
		options.onRun.mockReturnValue(pending.promise);
		const view = render(WorkflowLibraryView, options);
		await fireEvent.click(view.getByRole('button', { name: 'Run Workflow one' }));
		await view.rerender({ ...options, workflows: [workflow('one', { revision: 2 })] });
		pending.reject(new Error('old failure'));
		await waitFor(() => expect(view.queryByRole('alert')).toBeNull());
		await view.rerender({ ...options, disabled: true });
		expect(view.queryByRole('button', { name: 'Run Workflow one' })).toBeNull();
	});

	it('uses safe request feedback and retains the name on create failure, then closes on success', async () => {
		const options = props();
		options.onCreate.mockRejectedValueOnce(
			new DesktopRequestError({
				code: 'unavailable',
				message: 'Creation is unavailable. Try again.'
			})
		);
		const view = render(WorkflowLibraryView, options);
		await fireEvent.click(view.getByRole('button', { name: 'Create workflow' }));
		const name = view.getByRole('textbox', { name: 'Name' });
		await fireEvent.input(name, { target: { value: 'My new workflow' } });
		await fireEvent.submit(view.getByRole('textbox', { name: 'Name' }).closest('form')!);
		await waitFor(() =>
			expect(view.getByRole('alert').textContent?.trim()).toBe(
				'Creation is unavailable. Try again.'
			)
		);
		expect(name).toHaveProperty('value', 'My new workflow');
		options.onCreate.mockResolvedValue(undefined);
		await fireEvent.submit(view.getByRole('textbox', { name: 'Name' }).closest('form')!);
		await waitFor(() => expect(view.queryByRole('dialog')).toBeNull());
		expect(options.onCreate).toHaveBeenLastCalledWith('My new workflow');
	});

	it('allows cancelling create before submission', async () => {
		const view = render(WorkflowLibraryView, props());
		await fireEvent.click(view.getByRole('button', { name: 'Create workflow' }));
		await fireEvent.click(view.getByRole('button', { name: 'Cancel' }));
		expect(view.queryByRole('dialog')).toBeNull();
	});
});
