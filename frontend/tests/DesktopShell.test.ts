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
		detail: 'Recording'
	},
	{
		integration: 'twitch',
		connection: 'broadcaster',
		title: 'Twitch Broadcaster Account',
		state: 'connecting',
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
		expect(view.getByText('Connected 1/2')).toBeTruthy();
		expect(view.container.querySelector('summary .pip.connecting')).toBeTruthy();
		await view.rerender({
			connections: connections.map((item) => ({ ...item, state: 'connected' as const }))
		});
		expect(view.getByText('Connected 2/2')).toBeTruthy();
		expect(view.container.querySelector('summary .pip.connected')).toBeTruthy();
		await view.rerender({ connections: [{ ...connections[0], state: 'error' }] });
		expect(view.container.querySelector('summary .pip.error')).toBeTruthy();
		await view.rerender({ connections: [] });
		expect(view.getByText('Connected 0/0')).toBeTruthy();
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
		const summary = view.getByLabelText('Connections: 1 of 2 connected');
		await fireEvent.click(summary);
		expect(view.container.querySelector('details')).toHaveProperty('open', true);
		expect(view.getByText('Twitch Broadcaster Account')).toBeTruthy();
		expect(view.getByText('Reconnect for ad events')).toBeTruthy();
		await fireEvent.click(view.getAllByRole('button', { name: 'Configure' })[1]);
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
});
