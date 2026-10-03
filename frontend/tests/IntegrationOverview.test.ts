import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import IntegrationOverview from '../src/lib/IntegrationOverview.svelte';
import type { ConnectionStatus, WorkflowStatus } from '../src/lib/contracts/index';

afterEach(cleanup);

function workflow(
	id: string,
	overrides: Partial<WorkflowStatus> = {},
	usage: Record<string, string[]> = { obs: ['Scene switch', 'Set volume'] }
): WorkflowStatus {
	return {
		enabled: true,
		trigger_summary: '',
		step_count: 1,
		category: 'Automation',
		capabilities: [],
		capability_titles: {},
		id,
		title: `Automation ${id}`,
		revision: 1,
		has_steps: true,
		error: null,
		integration_usage: usage,
		...overrides
	};
}

const connection = (
	integration: string,
	connectionId: string,
	state: ConnectionStatus['state'] = 'connected'
): ConnectionStatus => ({
	integration,
	connection: connectionId,
	title: `Friendly ${connectionId}`,
	state,
	detail: 'Local connection detail'
});

describe('IntegrationOverview', () => {
	it('uses only generated integration metadata and opens the referenced workflow', async () => {
		const onOpen = vi.fn();
		const view = render(IntegrationOverview, {
			integration: 'obs',
			connections: [],
			workflows: [
				workflow('used', {}, { obs: ['Scene switch', 'Set volume'] }),
				workflow('other', {}, { obs_scene_switch: ['Ignored guessed label'] })
			],
			onOpen
		});

		expect(view.getByText('Scene switch · Set volume')).toBeTruthy();
		expect(view.queryByText('Ignored guessed label')).toBeNull();
		expect(view.queryByText('used')).toBeNull();
		await fireEvent.click(view.getByRole('button', { name: 'Automation used' }));
		expect(onOpen).toHaveBeenCalledExactlyOnceWith('used');
	});

	it('keeps disabled and unavailable workflow references visible with friendly status', () => {
		const view = render(IntegrationOverview, {
			integration: 'obs',
			connections: [],
			workflows: [
				workflow('disabled', { enabled: false }),
				workflow('broken', { error: 'private workflow error' })
			],
			onOpen: vi.fn()
		});

		expect(view.getByText('Disabled')).toBeTruthy();
		expect(view.getByText('Unavailable')).toBeTruthy();
		expect(view.queryByText('private workflow error')).toBeNull();
	});

	it('bounds rows to 50, paginates, clamps when rows are removed, and resets on integration change', async () => {
		const obs = Array.from({ length: 101 }, (_, index) => workflow(`obs-${index}`));
		const alt = Array.from({ length: 101 }, (_, index) =>
			workflow(`alt-${index}`, {}, { alt: ['Friendly feature'] })
		);
		const nextIntegration = Array.from({ length: 51 }, (_, index) =>
			workflow(`next-${index}`, {}, { next: ['Next feature'] })
		);
		const props = {
			integration: 'obs',
			connections: [] as ConnectionStatus[],
			workflows: obs,
			onOpen: vi.fn()
		};
		const view = render(IntegrationOverview, props);

		expect(view.getAllByRole('button', { name: /^Automation obs-/ })).toHaveLength(50);
		expect(view.getByText('Page 1 of 3')).toBeTruthy();
		await fireEvent.click(view.getByRole('button', { name: 'Next page' }));
		await fireEvent.click(view.getByRole('button', { name: 'Next page' }));
		expect(view.getByText('Page 3 of 3')).toBeTruthy();
		await view.rerender({ ...props, workflows: obs.slice(0, 20) });
		await waitFor(() =>
			expect(view.queryByRole('navigation', { name: 'Automation pages' })).toBeNull()
		);
		expect(view.getAllByRole('button', { name: /^Automation obs-/ })).toHaveLength(20);

		await view.rerender({ ...props, integration: 'alt', workflows: alt });
		await fireEvent.click(view.getByRole('button', { name: 'Next page' }));
		await fireEvent.click(view.getByRole('button', { name: 'Next page' }));
		expect(view.getByText('Page 3 of 3')).toBeTruthy();
		await view.rerender({ ...props, integration: 'next', workflows: nextIntegration });
		await waitFor(() => expect(view.getByText('Page 1 of 2')).toBeTruthy());
		expect(view.getAllByText('Next feature')).toHaveLength(50);
	});

	it('shows only matching connection statuses and the configured empty state', () => {
		const view = render(IntegrationOverview, {
			integration: 'obs',
			connections: [connection('obs', 'stream', 'connecting'), connection('vtube_studio', 'model')],
			workflows: [],
			onOpen: vi.fn()
		});

		expect(view.getByText('Friendly stream')).toBeTruthy();
		expect(view.getByText('Connecting')).toBeTruthy();
		expect(view.queryByText('Friendly model')).toBeNull();
		expect(view.queryByText('No connection configured.')).toBeNull();
		expect(view.getByText('No automations use this integration yet.')).toBeTruthy();

		view.rerender({ integration: 'vtube_studio', connections: [], workflows: [], onOpen: vi.fn() });
		expect(view.getByText('No connection configured.')).toBeTruthy();
	});
});
