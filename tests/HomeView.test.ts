import { fireEvent, render, waitFor } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import HomeView from '../src/lib/HomeView.svelte';
import { DesktopRequestError } from '../src/lib/desktop';
import HomeRenderFixture from './HomeRenderFixture.svelte';
import type { ConnectionStatus, HistoryPage, WorkflowStatus } from '../src/lib/contracts/index';

const connections: ConnectionStatus[] = [
	{
		integration: 'obs',
		connection: 'main',
		title: 'OBS Studio',
		state: 'connected',
		detail: 'Recording'
	},
	{
		integration: 'twitch',
		connection: 'bot',
		title: 'Twitch bot',
		state: 'error',
		detail: 'Reconnect required'
	}
];
const workflow = (overrides: Partial<WorkflowStatus> = {}): WorkflowStatus => ({
	id: 'welcome',
	title: 'Welcome message',
	enabled: true,
	trigger_summary: 'Chat message · !hello',
	step_count: 2,
	category: 'Chat',
	capabilities: [],
	capability_titles: {},
	integration_usage: {},
	revision: 3,
	has_steps: true,
	error: null,
	...overrides
});
const runSummary = (overrides = {}) => ({
	run_id: 'run-1',
	workflow_id: 'welcome',
	workflow_revision: 3,
	trigger: { kind: 'manual' as const, id: null },
	started_at_ms: 1_800_000_000_000,
	finished_at_ms: 1_800_000_000_500,
	outcome: { status: 'succeeded' as const },
	...overrides
});
const page: HistoryPage = {
	entries: [
		{ kind: 'record', summary: runSummary() },
		{
			kind: 'record',
			summary: runSummary({
				run_id: 'unknown',
				workflow_id: 'private-uuid',
				started_at_ms: 8_640_000_000_000_001
			})
		},
		{ kind: 'corrupt', file_name: '/private/path/private.json', message: 'internal parser detail' }
	],
	next_cursor: null
};
const baseProps = () => ({
	connections,
	workflows: [workflow()],
	history: page,
	onConfigure: vi.fn(),
	onNavigate: vi.fn(),
	onOpen: vi.fn(),
	onRun: vi.fn(async () => {}),
	onLibrary: vi.fn(),
	onHistory: vi.fn(),
	onRefreshHistory: vi.fn()
});

describe('Home view', () => {
	it('shows configured connections and routes each configure action', async () => {
		const props = baseProps();
		const view = render(HomeView, { props });
		expect(view.getByText('OBS Studio')).toBeTruthy();
		expect(view.getByText('Reconnect required')).toBeTruthy();
		await fireEvent.click(view.getAllByRole('button', { name: /Configure/ })[1]);
		expect(props.onConfigure).toHaveBeenCalledWith(connections[1]);
	});

	it('bounds automation and run lists and keeps unknown or corrupt history private', async () => {
		const workflows = Array.from({ length: 8 }, (_, index) =>
			workflow({ id: `wf-${index}`, title: `Workflow ${index}` })
		);
		const props = { ...baseProps(), workflows };
		const view = render(HomeView, { props });
		expect(view.getAllByRole('button', { name: /^Open Workflow/ })).toHaveLength(6);
		expect(view.getAllByRole('button', { name: /^View .* run$/ })).toHaveLength(2);
		expect(view.getAllByText('Unavailable workflow')).toHaveLength(2);
		expect(view.getByText('Unavailable run record')).toBeTruthy();
		expect(view.container.textContent).not.toContain('private-uuid');
		expect(view.container.textContent).not.toContain('internal parser detail');
		expect(view.container.textContent).not.toContain('private.json');
		await fireEvent.click(view.getByRole('button', { name: 'View unavailable run record' }));
		expect(props.onHistory).toHaveBeenCalledWith();
	});

	it('routes empty connections to setup and view all to the library', async () => {
		const props = { ...baseProps(), connections: [] };
		const view = render(HomeView, { props });
		await fireEvent.click(view.getByRole('button', { name: 'Set up a connection' }));
		await fireEvent.click(view.getByRole('button', { name: 'View all' }));
		expect(props.onNavigate).toHaveBeenCalledWith('streaming');
		expect(props.onLibrary).toHaveBeenCalledOnce();
	});

	it('disables ineligible runs, prevents duplicate starts, and surfaces only safe request errors', async () => {
		let rejectRun: (error: unknown) => void = () => {};
		const pending = new Promise<void>((_, reject) => {
			rejectRun = reject;
		});
		const props = {
			...baseProps(),
			workflows: [
				workflow(),
				workflow({ id: 'disabled', title: 'Disabled', enabled: false }),
				workflow({ id: 'empty', title: 'Empty', has_steps: false })
			],
			onRun: vi.fn(() => pending)
		};
		const view = render(HomeView, { props });
		expect(view.getByRole('button', { name: 'Run Disabled' })).toHaveProperty('disabled', true);
		expect(view.getByRole('button', { name: 'Run Empty' })).toHaveProperty('disabled', true);
		const runButton = view.getByRole('button', { name: 'Run Welcome message' });
		await fireEvent.click(runButton);
		await fireEvent.click(runButton);
		expect(props.onRun).toHaveBeenCalledTimes(1);
		rejectRun(new DesktopRequestError('The automation could not start.'));
		await waitFor(() =>
			expect(view.getByRole('alert').textContent).toContain('could not complete')
		);
		await fireEvent.click(view.getByRole('button', { name: 'Dismiss error for Welcome message' }));
		expect(view.queryByRole('alert')).toBeNull();
	});

	it('uses generic failures and discards feedback when the workflow changes or disappears', async () => {
		const props = baseProps();
		props.onRun = vi.fn().mockRejectedValue(new Error('/private/config/keychain'));
		const view = render(HomeView, { props });
		await fireEvent.click(view.getByRole('button', { name: 'Run Welcome message' }));
		await waitFor(() =>
			expect(view.getByRole('alert').textContent).toContain('could not be started')
		);
		expect(view.container.textContent).not.toContain('/private/config/keychain');
		await view.rerender({ ...props, workflows: [workflow({ revision: 4 })] });
		expect(view.queryByRole('alert')).toBeNull();
		props.onRun = vi.fn().mockRejectedValue(new Error('private'));
		await fireEvent.click(view.getByRole('button', { name: 'Run Welcome message' }));
		await waitFor(() => expect(view.getByRole('alert')).toBeTruthy());
		await view.rerender({ ...props, workflows: [] });
		expect(view.queryByRole('alert')).toBeNull();
	});

	it('shows history loading and retry states, then navigates to a selected run', async () => {
		const props = { ...baseProps(), history: null, historyLoading: true };
		const view = render(HomeView, { props });
		expect(view.getByRole('status').textContent).toContain('Loading runs');
		await view.rerender({
			...props,
			historyLoading: false,
			historyError: 'History is unavailable.'
		});
		await fireEvent.click(view.getByRole('button', { name: 'Try again' }));
		expect(props.onRefreshHistory).toHaveBeenCalledOnce();
		await view.rerender({ ...props, history: page, historyError: '' });
		await fireEvent.click(view.getByRole('button', { name: 'View Welcome message run' }));
		expect(props.onHistory).toHaveBeenCalledWith('run-1');
	});

	it('keeps cached runs visible during a failed refresh', () => {
		const view = render(HomeView, {
			props: { ...baseProps(), historyError: 'History could not refresh.' }
		});
		expect(view.getByRole('alert').textContent).toBe('History could not refresh.');
		expect(view.getByRole('button', { name: 'View Welcome message run' })).toBeTruthy();
		expect(view.getByText(/Unknown time/)).toBeTruthy();
	});

	it('renders inside the desktop shell with its shared navigation and connection overview', () => {
		const view = render(HomeRenderFixture);
		expect(view.getByRole('heading', { name: 'Home' })).toBeTruthy();
		expect(view.getByRole('navigation', { name: 'Main navigation' })).toBeTruthy();
		expect(view.getByRole('button', { name: 'Open A little welcome' })).toBeTruthy();
	});
});
