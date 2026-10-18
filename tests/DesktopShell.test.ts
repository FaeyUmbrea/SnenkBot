import { fireEvent, render, waitFor } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import type { ComponentProps } from 'svelte';
import DesktopShell from '../src/lib/DesktopShell.svelte';
import ConnectionStatus from '../src/lib/ConnectionStatus.svelte';
import ShellFixture from './ShellFixture.svelte';
import type { ConnectionStatus as Connection } from '../src/lib/contracts/index';

const connections: Connection[] = [
	{
		integration: 'obs',
		connection: 'main',
		title: 'OBS Studio',
		state: 'connected',
		show_when_healthy: true,
		state_label: 'Connected',
		detail: 'Recording'
	},
	{
		integration: 'twitch',
		connection: 'broadcaster',
		title: 'Twitch Broadcaster Account',
		state: 'connecting',
		show_when_healthy: true,
		state_label: 'Connecting',
		detail: ''
	}
];

function props(): Omit<ComponentProps<typeof DesktopShell>, 'children'> {
	return {
		page: 'automations',
		title: 'Sunday stream',
		connections,
		errors: [{ id: 'notice', message: 'Reconnect the broadcaster account.' }],
		onNavigate: vi.fn(),
		onConfigure: vi.fn(),
		onDismissError: vi.fn(),
		onBack: vi.fn()
	};
}

describe('Desktop shell', () => {
	it('keeps navigation split and reports active context without destroying the active view', async () => {
		const callbacks = props();
		const view = render(ShellFixture, { props: { props: callbacks } });
		expect(view.getByRole('button', { name: 'Automations' }).getAttribute('aria-current')).toBe(
			'page'
		);
		await fireEvent.input(view.getByLabelText('Unsaved title'), {
			target: { value: 'An unfinished edit' }
		});
		await fireEvent.click(view.getByRole('button', { name: 'Broadcast Apps' }));
		expect(callbacks.onNavigate).toHaveBeenCalledWith('broadcast');
		expect(view.getByLabelText('Unsaved title')).toHaveProperty('value', 'An unfinished edit');
		await fireEvent.click(view.getByRole('button', { name: 'Back to previous view' }));
		expect(callbacks.onBack).toHaveBeenCalledOnce();
		expect(view.container.querySelector('.navigation-group.settings')?.textContent).toContain(
			'General'
		);
	});
	it('makes notices dismissible from the editor and preserves failed dismissals', async () => {
		let reject: (error: unknown) => void = () => {};
		const pending = new Promise<void>((_, fail) => {
			reject = fail;
		});
		const callbacks = { ...props(), onDismissError: vi.fn(() => pending) };
		const view = render(ShellFixture, { props: { props: callbacks } });
		const button = view.getByRole('button', { name: 'Dismiss notice' });
		await fireEvent.click(button);
		expect(button).toHaveProperty('disabled', true);
		expect(callbacks.onDismissError).toHaveBeenCalledWith('notice');
		reject(new Error('Internal storage path'));
		await waitFor(() => expect(view.getByRole('alert').textContent).toContain('Try again'));
		expect(button).toHaveProperty('disabled', false);
		expect(view.getByRole('status').textContent).toContain('Reconnect the broadcaster');
		expect(view.container.textContent).not.toContain('Internal storage path');
	});
});

describe('Connection status', () => {
	it('counts the supplied configured facets and changes severity as their state changes', async () => {
		const view = render(ConnectionStatus, { connections, onConfigure: vi.fn() });
		expect(view.getByText('Connections 1/2')).toBeTruthy();
		expect(view.container.querySelector('summary .pip.connecting')).toBeTruthy();
		await view.rerender({
			connections: connections.map((item) => ({ ...item, state: 'connected' as const }))
		});
		expect(view.getByText('Connections 2/2')).toBeTruthy();
		expect(view.container.querySelector('summary .pip.connected')).toBeTruthy();
		await view.rerender({ connections: [{ ...connections[0], state: 'error' }] });
		expect(view.container.querySelector('summary .pip.error')).toBeTruthy();
		await view.rerender({ connections: [] });
		expect(view.getByText('Connections 0/0')).toBeTruthy();
		expect(view.container.querySelector('summary .pip.inactive')).toBeTruthy();
	});
	it('opens detailed states, routes recovery by facet and closes on Escape or outside pointer', async () => {
		const onConfigure = vi.fn();
		const view = render(ConnectionStatus, {
			connections,
			onConfigure,
			requests: [
				{
					integration: 'twitch',
					connection: 'broadcaster',
					reason: 'Reconnect for ad events',
					affected_features: []
				}
			]
		});
		const summary = view.getByLabelText('Connections: 1 of 2 healthy');
		await fireEvent.click(summary);
		expect(view.container.querySelector('details')).toHaveProperty('open', true);
		expect(view.getByText('Twitch Broadcaster Account')).toBeTruthy();
		expect(view.getByText('Reconnect for ad events')).toBeTruthy();
		await fireEvent.click(
			view.getByRole('button', { name: 'Configure Twitch Broadcaster Account' })
		);
		expect(onConfigure).toHaveBeenCalledWith(connections[1]);
		expect(view.container.querySelector('details')).toHaveProperty('open', false);
		await fireEvent.click(summary);
		await fireEvent.keyDown(window, { key: 'Escape' });
		expect(document.activeElement).toBe(summary);
		expect(view.container.querySelector('details')).toHaveProperty('open', false);
		await fireEvent.click(summary);
		await fireEvent.pointerDown(document.body);
		expect(view.container.querySelector('details')).toHaveProperty('open', false);
	});
	it('keeps healthy rows compact while showing error detail and recovery prompts', async () => {
		const view = render(ConnectionStatus, {
			connections: [
				connections[0],
				{
					...connections[1],
					state: 'error' as const,
					detail: 'Reconnect the broadcaster account.'
				}
			],
			requests: [
				{
					integration: 'twitch',
					connection: 'broadcaster',
					reason: 'Ad events need new permission',
					affected_features: []
				}
			],
			onConfigure: vi.fn()
		});
		await fireEvent.click(view.getByLabelText('Connections: 1 of 2 healthy'));
		expect(view.queryByText('Recording')).toBeNull();
		expect(view.getByText('Reconnect the broadcaster account.')).toBeTruthy();
		expect(view.getByText('Ad events need new permission')).toBeTruthy();
		const healthy = view.getByText('OBS Studio').closest('li');
		expect(healthy?.textContent).toMatch(/OBS Studio\s+Connected/);
		expect(healthy?.querySelectorAll('p')).toHaveLength(0);
		expect(view.getByRole('button', { name: 'Configure OBS Studio' })).toBeTruthy();
	});
});

it('uses owner-defined state labels and puts errors before recovery and healthy services', async () => {
	const rows: Connection[] = [
		{ ...connections[0], integration: 'api', title: 'API server', state_label: 'Running' },
		{
			...connections[1],
			integration: 'obs',
			title: 'OBS Studio',
			state: 'error',
			show_when_healthy: true,
			state_label: 'Unavailable',
			detail: 'Cannot reach OBS.'
		},
		{ ...connections[1], integration: 'other', title: 'Another service' }
	];
	const view = render(ConnectionStatus, { connections: rows, onConfigure: vi.fn() });
	await fireEvent.click(view.getByLabelText('Connections: 1 of 3 healthy'));
	const titles = [...view.container.querySelectorAll('li strong')].map((node) => node.textContent);
	expect(titles).toEqual(['OBS Studio', 'Another service', 'API server']);
	expect(view.getByText('Running')).toBeTruthy();
	expect(view.getByText('Unavailable')).toBeTruthy();
	expect(view.queryByText('Recording')).toBeNull();
});

it('keeps background services quiet until they need attention and removes them after recovery', async () => {
	const lookup: Connection = {
		...connections[0],
		integration: 'new-lookup-provider',
		connection: 'lookup',
		title: 'Background lookup',
		show_when_healthy: false,
		state_label: 'Ready'
	};
	const view = render(ConnectionStatus, {
		connections: [...connections, lookup],
		onConfigure: vi.fn()
	});
	await fireEvent.click(view.getByLabelText('Connections: 1 of 2 healthy'));
	expect(view.queryByText('Background lookup')).toBeNull();
	expect(view.getByText('OBS Studio')).toBeTruthy();

	await view.rerender({
		connections: [...connections, { ...lookup, state: 'error', state_label: 'Unavailable' }]
	});
	expect(view.getByLabelText('Connections: 1 of 3 healthy')).toBeTruthy();
	expect(view.container.querySelector('li strong')?.textContent).toBe('Background lookup');

	await view.rerender({ connections: [...connections, lookup] });
	expect(view.queryByText('Background lookup')).toBeNull();
	expect(view.getByLabelText('Connections: 1 of 2 healthy')).toBeTruthy();

	await view.rerender({
		requests: [
			{
				integration: lookup.integration,
				connection: lookup.connection,
				reason: 'Update lookup credentials',
				affected_features: []
			}
		]
	});
	expect(view.getByText('Background lookup')).toBeTruthy();
	expect(view.getByText('Update lookup credentials')).toBeTruthy();

	await view.rerender({ connections: [lookup], requests: [] });
	expect(view.getByLabelText('Connections: 0 of 0 healthy')).toBeTruthy();
	expect(view.getByText('No connections need attention.')).toBeTruthy();
});
