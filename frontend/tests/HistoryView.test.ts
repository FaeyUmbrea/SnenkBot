import { fireEvent, render, waitFor } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import HistoryView from '../src/lib/HistoryView.svelte';
import { DesktopRequestError } from '../src/lib/desktop';
import type { HistoryPage, RunRecord_Serialize, StepTrace } from '../src/lib/contracts/index';
import { schemas } from './fixture';

function record(run_id = 'run-one'): RunRecord_Serialize {
	return {
		run_id,
		workflow_id: 'workflow-id',
		workflow_revision: 2,
		trigger: { kind: 'event', id: 'raw-trigger-id' },
		started_at_ms: 1_790_000_000_000,
		finished_at_ms: 1_790_000_000_500,
		outcome: { status: 'succeeded' },
		step_trace: []
	};
}
function page(run_id = 'run-one', next_cursor: string | null = null): HistoryPage {
	return { entries: [{ kind: 'record', summary: record(run_id) }], next_cursor };
}
function deferred<T>() {
	let resolve!: (value: T) => void;
	let reject!: (failure: unknown) => void;
	const promise = new Promise<T>((yes, no) => {
		resolve = yes;
		reject = no;
	});
	return { promise, resolve, reject };
}

describe('History view', () => {
	it('opens requested runs outside the current page and rejects stale route replies', async () => {
		const first = deferred<RunRecord_Serialize>();
		const inspectRun = vi
			.fn()
			.mockReturnValueOnce(first.promise)
			.mockResolvedValueOnce(record('next'));
		const props = {
			loadPage: vi.fn().mockResolvedValue(page()),
			inspectRun,
			requestedRunId: 'not-on-page',
			onSelectRun: vi.fn()
		};
		const view = render(HistoryView, props);
		await waitFor(() => expect(inspectRun).toHaveBeenCalledWith({ run_id: 'not-on-page' }));
		await view.rerender({ ...props, requestedRunId: 'next' });
		await waitFor(() => expect(view.getByText('Saved version')).toBeTruthy());
		first.reject(new Error('private late error'));
		await Promise.resolve();
		expect(view.queryByRole('alert')).toBeNull();
		await fireEvent.click(view.getByRole('button', { name: 'Close run details' }));
		expect(props.onSelectRun).toHaveBeenCalledWith(null);
		expect(view.queryByRole('complementary')).toBeNull();
		await view.rerender({ ...props, requestedRunId: null });
		expect(view.queryByRole('complementary')).toBeNull();
		expect(inspectRun).toHaveBeenCalledTimes(2);
	});
	it('uses bounded queries and keeps the browsed page after inspecting and returning', async () => {
		const loadPage = vi
			.fn()
			.mockResolvedValueOnce(page('new', 'older-page'))
			.mockResolvedValueOnce(page('old'))
			.mockResolvedValueOnce(page('new', 'older-page'));
		const inspectRun = vi.fn().mockResolvedValue(record('old'));
		const view = render(HistoryView, {
			loadPage,
			inspectRun,
			workflowTitles: { 'workflow-id': 'Sunday stream' }
		});
		await waitFor(() =>
			expect(view.getByRole('button', { name: 'Older' })).toHaveProperty('disabled', false)
		);
		expect(loadPage).toHaveBeenLastCalledWith({
			page_size: 50,
			cursor: null,
			filter: { workflow_id: null, outcome: null, trigger_kind: null }
		});
		await fireEvent.click(view.getByRole('button', { name: 'Older' }));
		await waitFor(() =>
			expect(view.getByRole('button', { name: 'Newer' })).toHaveProperty('disabled', false)
		);
		expect(loadPage.mock.calls[1][0].cursor).toBe('older-page');
		await fireEvent.click(view.getByRole('button', { name: 'Inspect Sunday stream run' }));
		await waitFor(() =>
			expect(view.getByRole('complementary', { name: 'Run details' })).toBeTruthy()
		);
		await waitFor(() => expect(view.getByText('Saved version')).toBeTruthy());
		expect(inspectRun).toHaveBeenCalledWith({ run_id: 'old' });
		expect(view.container.textContent).not.toContain('raw-trigger-id');
		await fireEvent.click(view.getByRole('button', { name: 'Close run details' }));
		expect(loadPage).toHaveBeenCalledTimes(2);
		await fireEvent.click(view.getByRole('button', { name: 'Newer' }));
		await waitFor(() => expect(loadPage).toHaveBeenCalledTimes(3));
		expect(loadPage.mock.calls[2][0].cursor).toBeNull();
	});
	it('rejects old page responses after a filter change', async () => {
		const first = deferred<HistoryPage>();
		const next = deferred<HistoryPage>();
		const loadPage = vi.fn().mockReturnValueOnce(first.promise).mockReturnValueOnce(next.promise);
		const view = render(HistoryView, {
			loadPage,
			inspectRun: vi.fn(),
			workflowTitles: { 'workflow-id': 'Filtered workflow' }
		});
		await fireEvent.change(view.getByLabelText('Filter run results'), {
			target: { value: 'failed' }
		});
		next.resolve({ entries: [], next_cursor: null });
		await waitFor(() => expect(view.getByText('No runs match this view.')).toBeTruthy());
		first.resolve(page());
		await Promise.resolve();
		expect(view.queryByRole('button', { name: 'Inspect Filtered workflow run' })).toBeNull();
		expect(loadPage.mock.calls[1][0].filter.outcome).toBe('failed');
	});
	it('retains the current page on query failure and provides an explicit refresh', async () => {
		const loadPage = vi
			.fn()
			.mockResolvedValueOnce(page('one', 'cursor'))
			.mockRejectedValueOnce(
				new DesktopRequestError({
					code: 'stale_cursor',
					message: 'Run history changed. Refresh to continue.'
				})
			)
			.mockResolvedValueOnce(page());
		const view = render(HistoryView, {
			loadPage,
			inspectRun: vi.fn(),
			workflowTitles: { 'workflow-id': 'Sunday stream' }
		});
		await waitFor(() =>
			expect(view.getByRole('button', { name: 'Older' })).toHaveProperty('disabled', false)
		);
		await fireEvent.click(view.getByRole('button', { name: 'Older' }));
		await waitFor(() => expect(view.getByRole('alert').textContent).toContain('Refresh'));
		expect(view.getByRole('button', { name: 'Inspect Sunday stream run' })).toBeTruthy();
		await fireEvent.click(view.getByRole('button', { name: 'Refresh' }));
		await waitFor(() => expect(view.queryByRole('alert')).toBeNull());
		expect(loadPage.mock.calls[2][0].cursor).toBeNull();
	});
	it('rejects stale inspection callbacks after closing or choosing another run', async () => {
		const first = deferred<RunRecord_Serialize>();
		const inspectRun = vi
			.fn()
			.mockReturnValueOnce(first.promise)
			.mockResolvedValueOnce(record('run-two'));
		const view = render(HistoryView, {
			loadPage: vi.fn().mockResolvedValue({
				entries: [
					{ kind: 'record', summary: record() },
					{ kind: 'record', summary: record('run-two') }
				],
				next_cursor: null
			}),
			inspectRun,
			workflowTitles: { 'workflow-id': 'Sunday stream' }
		});
		await waitFor(() =>
			expect(view.getAllByRole('button', { name: 'Inspect Sunday stream run' })).toHaveLength(2)
		);
		await fireEvent.click(view.getAllByRole('button', { name: 'Inspect Sunday stream run' })[0]);
		await fireEvent.click(view.getAllByRole('button', { name: 'Inspect Sunday stream run' })[1]);
		await waitFor(() => expect(view.getByText('Saved version')).toBeTruthy());
		first.reject(new Error('private diagnostic'));
		await Promise.resolve();
		expect(view.queryByRole('alert')).toBeNull();
		await fireEvent.click(view.getByRole('button', { name: 'Close run details' }));
		expect(view.queryByRole('complementary')).toBeNull();
	});
	it('shows corrupt records and out-of-range timestamps without exposing IDs or crashing', async () => {
		const invalidTime = record();
		invalidTime.started_at_ms = 9e18;
		const view = render(HistoryView, {
			loadPage: vi.fn().mockResolvedValue({
				entries: [
					{ kind: 'record', summary: invalidTime },
					{
						kind: 'corrupt',
						file_name: 'private-file.json',
						message: 'This run record could not be read.'
					}
				],
				next_cursor: null
			}),
			inspectRun: vi.fn()
		});
		await waitFor(() => expect(view.getByText('Unknown time')).toBeTruthy());
		expect(view.getByText('Unreadable run')).toBeTruthy();
		expect(view.container.textContent).not.toContain('private-file.json');
		expect(view.container.textContent).not.toContain('workflow-id');
	});
	it('renders only one trace page and explains failure policy without exposing capabilities', async () => {
		const trace: StepTrace = {
			sequence: 1,
			parent_sequence: null,
			workflow_id: 'workflow-id',
			workflow_revision: 2,
			step_id: 'private-step',
			kind: { kind: 'action', capability: schemas[0].id, version: schemas[0].version },
			started_at_ms: 0,
			finished_at_ms: 50,
			duration_ms: 50,
			outcome: {
				status: 'failed',
				kind: 'connector_unavailable',
				remote_effect_uncertain: true,
				continued_by_policy: true
			}
		};
		const detail = {
			...record(),
			step_trace: Array.from({ length: 120 }, (_, index) => ({ ...trace, sequence: index + 1 }))
		};
		const view = render(HistoryView, {
			loadPage: vi.fn().mockResolvedValue(page()),
			inspectRun: vi.fn().mockResolvedValue(detail),
			schemas,
			workflowTitles: { 'workflow-id': 'Sunday stream' }
		});
		await waitFor(() =>
			expect(view.getByRole('button', { name: 'Inspect Sunday stream run' })).toBeTruthy()
		);
		await fireEvent.click(view.getByRole('button', { name: 'Inspect Sunday stream run' }));
		await waitFor(() => expect(view.getAllByText('Set stream title')).toHaveLength(50));
		expect(view.getAllByText(/remote change may have happened/)).toHaveLength(50);
		expect(view.container.textContent).not.toContain(schemas[0].id);
		expect(view.container.textContent).not.toContain('private-step');
		await fireEvent.click(view.getByRole('button', { name: 'Next steps' }));
		expect(view.getAllByText('Set stream title')).toHaveLength(50);
		await fireEvent.click(view.getByRole('button', { name: 'Next steps' }));
		expect(view.getAllByText('Set stream title')).toHaveLength(20);
	});
});
