import { describe, expect, it, vi } from 'vitest';
import { subscribeDesktopUpdates, type DesktopFeedTransport } from '../src/lib/desktop-feed';
import type { DesktopBatch, DesktopSnapshot, DesktopEvent } from '../src/lib/contracts/index';

function deferred<T>() {
	let resolve!: (value: T) => void;
	let reject!: (error: unknown) => void;
	const promise = new Promise<T>((accept, fail) => {
		resolve = accept;
		reject = fail;
	});
	return { promise, resolve, reject };
}

function snapshot(sequence = 0): DesktopSnapshot {
	return {
		sequence,
		startup: { state: 'starting' },
		workflows: [],
		connections: [],
		reconfiguration_requests: [],
		errors: [],
		last_completed: null,
		pending_input: null,
		twitch_authentication: null
	};
}

function harness() {
	let receive!: (batch: DesktopBatch) => void;
	const stop = vi.fn();
	const onSnapshot = vi.fn();
	const onEngineEvent = vi.fn();
	const onError = vi.fn();
	const transport = {
		listen: vi.fn<DesktopFeedTransport['listen']>().mockImplementation(async (handler) => {
			receive = handler;
			return stop;
		}),
		snapshot: vi.fn<DesktopFeedTransport['snapshot']>().mockResolvedValue(snapshot())
	};
	const feed = subscribeDesktopUpdates({ onSnapshot, onEngineEvent, onError }, transport);
	function send(sequence: number, event: DesktopEvent) {
		receive({ updates: [{ sequence, event }], resync_required: false });
	}
	return {
		feed,
		transport,
		receive: (batch: DesktopBatch) => receive(batch),
		send,
		stop,
		onSnapshot,
		onEngineEvent,
		onError
	};
}

describe('desktop update subscription', () => {
	it('keeps the current sign-in in reconnect snapshots without accepting older events', async () => {
		const test = harness();
		await test.feed.ready;
		const attempt = {
			attempt_id: 'attempt',
			role: 'bot',
			phase: { state: 'review', data: { login: 'bot', user_id: '123' } }
		} as const;
		test.send(1, { event: 'twitch_authentication', payload: attempt });
		expect(test.onSnapshot.mock.lastCall?.[0].twitch_authentication).toEqual(attempt);
		test.send(1, {
			event: 'twitch_authentication',
			payload: { ...attempt, phase: { state: 'starting' } }
		});
		expect(test.onSnapshot.mock.lastCall?.[0].twitch_authentication).toEqual(attempt);
		test.transport.snapshot.mockResolvedValue({ ...snapshot(1), twitch_authentication: attempt });
		await test.feed.refresh();
		expect(test.onSnapshot.mock.lastCall?.[0].twitch_authentication).toEqual(attempt);
		test.feed.dispose();
	});

	it('attaches before taking a snapshot, buffering newer events and discarding duplicates', async () => {
		const initial = deferred<DesktopSnapshot>();
		const test = harness();
		test.transport.snapshot.mockReturnValue(initial.promise);
		expect(test.transport.snapshot).not.toHaveBeenCalled();
		await Promise.resolve();
		test.send(1, { event: 'lifecycle', payload: { state: 'ready' } });
		test.send(2, { event: 'lifecycle', payload: { state: 'closing' } });
		expect(test.onSnapshot).not.toHaveBeenCalled();
		initial.resolve({ ...snapshot(1), startup: { state: 'ready' } });
		await test.feed.ready;
		expect(test.onSnapshot.mock.calls.map(([state]) => state.sequence)).toEqual([1, 2]);
		test.send(2, { event: 'lifecycle', payload: { state: 'failed', message: 'stale' } });
		expect(test.onSnapshot).toHaveBeenCalledTimes(2);
		expect(test.onSnapshot.mock.lastCall?.[0].startup).toEqual({ state: 'closing' });
		test.feed.dispose();
	});

	it('coalesces lag recovery while preserving snapshot sequence and replaying later updates', async () => {
		const test = harness();
		await test.feed.ready;
		const first = deferred<DesktopSnapshot>();
		const second = deferred<DesktopSnapshot>();
		test.transport.snapshot.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
		test.receive({ updates: [], resync_required: true });
		test.receive({ updates: [], resync_required: true });
		test.receive({ updates: [], resync_required: true });
		test.send(4, { event: 'lifecycle', payload: { state: 'closing' } });
		expect(test.transport.snapshot).toHaveBeenCalledTimes(2);
		first.resolve({ ...snapshot(2), startup: { state: 'ready' } });
		await Promise.resolve();
		expect(test.onSnapshot.mock.lastCall?.[0].sequence).toBe(2);
		expect(test.transport.snapshot).toHaveBeenCalledTimes(3);
		second.resolve(snapshot(3));
		await test.feed.refresh();
		expect(test.onSnapshot.mock.calls.map(([state]) => state.sequence)).toEqual([0, 2, 3, 4]);
		expect(test.transport.snapshot).toHaveBeenCalledTimes(3);
		test.feed.dispose();
	});

	it('does not let delayed snapshots overwrite already applied state', async () => {
		const test = harness();
		test.transport.snapshot.mockResolvedValue(snapshot(10));
		await test.feed.ready;
		test.send(11, { event: 'lifecycle', payload: { state: 'ready' } });
		test.transport.snapshot.mockResolvedValue(snapshot(3));
		await test.feed.refresh();
		expect(test.onSnapshot).toHaveBeenCalledTimes(2);
		expect(test.onSnapshot.mock.lastCall?.[0].sequence).toBe(11);
		test.feed.dispose();
	});

	it('refreshes a sequence gap without applying partial state and buffers later updates', async () => {
		const test = harness();
		await test.feed.ready;
		test.send(1, { event: 'lifecycle', payload: { state: 'ready' } });
		const recovery = deferred<DesktopSnapshot>();
		test.transport.snapshot.mockReturnValueOnce(recovery.promise);
		test.send(3, { event: 'error_added', payload: { id: 'three', message: 'error' } });
		test.send(4, { event: 'lifecycle', payload: { state: 'closing' } });
		test.send(5, {
			event: 'engine',
			payload: { StepStarted: { workflow_id: 'workflow', step_id: 'step' } }
		});
		const refresh = test.feed.refresh();
		expect(test.transport.snapshot).toHaveBeenCalledTimes(2);
		expect(test.onSnapshot.mock.lastCall?.[0].sequence).toBe(1);
		expect(test.onEngineEvent).not.toHaveBeenCalled();
		recovery.resolve({
			...snapshot(3),
			startup: { state: 'ready' },
			errors: [{ id: 'three', message: 'error' }]
		});
		await refresh;
		expect(test.onSnapshot.mock.calls.map(([state]) => state.sequence)).toEqual([0, 1, 3, 4, 5]);
		expect(test.transport.snapshot).toHaveBeenCalledTimes(2);
		expect(test.onSnapshot.mock.lastCall?.[0].errors).toHaveLength(1);
		expect(test.onSnapshot.mock.lastCall?.[0].startup).toEqual({ state: 'closing' });
		expect(test.onEngineEvent).toHaveBeenCalledTimes(1);
		test.feed.dispose();
	});

	it('reduces input and completion state, keeping only 32 errors and original engine events', async () => {
		const test = harness();
		await test.feed.ready;
		const input = {
			request_id: 'current',
			title: 'Input',
			fields: [],
			defaults: {},
			values: {},
			validating: false,
			error: null
		};
		test.send(1, { event: 'input', payload: { event: 'requested', payload: input } });
		test.send(2, {
			event: 'input',
			payload: { event: 'closed', payload: { request_id: 'previous' } }
		});
		expect(test.onSnapshot.mock.lastCall?.[0].pending_input).toEqual(input);
		test.send(3, {
			event: 'input',
			payload: { event: 'changed', payload: { ...input, validating: true } }
		});
		expect(test.onSnapshot.mock.lastCall?.[0].pending_input.validating).toBe(true);
		test.send(4, {
			event: 'input',
			payload: { event: 'closed', payload: { request_id: 'current' } }
		});
		expect(test.onSnapshot.mock.lastCall?.[0].pending_input).toBeNull();
		for (let index = 0; index < 40; index++) {
			test.send(index + 5, {
				event: 'error_added',
				payload: { id: `${index}`, message: `error ${index}` }
			});
		}
		expect(test.onSnapshot.mock.lastCall?.[0].errors).toHaveLength(32);
		expect(test.onSnapshot.mock.lastCall?.[0].errors[0].id).toBe('8');
		test.send(45, { event: 'error_dismissed', payload: { id: '8' } });
		expect(test.onSnapshot.mock.lastCall?.[0].errors).toHaveLength(31);
		const engine = { StepStarted: { workflow_id: 'workflow', step_id: 'step' } };
		test.send(46, { event: 'engine', payload: engine });
		expect(test.onEngineEvent).toHaveBeenCalledWith(engine);
		expect(test.onSnapshot.mock.lastCall?.[0].last_completed).toBeNull();
		const completed = {
			run_id: 'run',
			workflow_id: 'workflow',
			revision: 1,
			outcome: 'Success' as const
		};
		test.send(47, { event: 'run_completed', payload: completed });
		expect(test.onSnapshot.mock.lastCall?.[0].last_completed).toEqual(completed);
		test.feed.dispose();
	});

	it('replaces an overflowing bootstrap buffer with another snapshot', async () => {
		const test = harness();
		const initial = deferred<DesktopSnapshot>();
		test.transport.snapshot.mockReturnValueOnce(initial.promise).mockResolvedValue(snapshot(300));
		await Promise.resolve();
		for (let sequence = 1; sequence <= 300; sequence++) {
			test.send(sequence, {
				event: 'engine',
				payload: { StepStarted: { workflow_id: 'workflow', step_id: 'step' } }
			});
		}
		initial.resolve(snapshot());
		await test.feed.ready;
		expect(test.transport.snapshot).toHaveBeenCalledTimes(2);
		expect(test.onSnapshot.mock.lastCall?.[0].sequence).toBe(300);
		expect(test.onEngineEvent).not.toHaveBeenCalled();
		test.feed.dispose();
	});

	it('replaces inventory and connection projections from typed events', async () => {
		const test = harness();
		await test.feed.ready;
		const workflows: DesktopSnapshot['workflows'] = [
			{
				id: 'workflow',
				title: 'Workflow',
				revision: 2,
				enabled: true,
				trigger_summary: 'Manual',
				step_count: 1,
				category: 'Automation',
				capabilities: [],
				capability_titles: {},
				integration_usage: {},
				has_steps: true,
				error: null
			}
		];
		const connections: DesktopSnapshot['connections'] = [
			{
				integration: 'obs',
				connection: 'main',
				title: 'OBS',
				state: 'connected',
				detail: ''
			}
		];
		const requests: DesktopSnapshot['reconfiguration_requests'] = [
			{
				integration: 'obs',
				connection: 'main',
				reason: 'Update settings',
				affected_features: []
			}
		];
		test.send(1, { event: 'inventory', payload: workflows });
		test.send(2, { event: 'connections', payload: { connections, requests } });
		expect(test.onSnapshot.mock.lastCall?.[0]).toMatchObject({
			workflows,
			connections,
			reconfiguration_requests: requests,
			sequence: 2
		});
		test.feed.dispose();
	});

	it('rejects listener failure without taking a snapshot', async () => {
		const transport = {
			listen: vi.fn().mockRejectedValue(new Error('Subscription unavailable')),
			snapshot: vi.fn()
		};
		const feed = subscribeDesktopUpdates({ onSnapshot: vi.fn() }, transport);
		await expect(feed.ready).rejects.toThrow('Subscription unavailable');
		expect(transport.snapshot).not.toHaveBeenCalled();
		feed.dispose();
	});

	it('unsubscribes exactly once when disposed during listener registration', async () => {
		const listener = deferred<() => void>();
		const stop = vi.fn();
		const transport = { listen: vi.fn().mockReturnValue(listener.promise), snapshot: vi.fn() };
		const onSnapshot = vi.fn();
		const feed = subscribeDesktopUpdates({ onSnapshot }, transport);
		feed.dispose();
		listener.resolve(stop);
		await feed.ready;
		feed.dispose();
		expect(stop).toHaveBeenCalledTimes(1);
		expect(transport.snapshot).not.toHaveBeenCalled();
		expect(onSnapshot).not.toHaveBeenCalled();
	});

	it('ignores a slow snapshot and subsequent events after disposal', async () => {
		const test = harness();
		const initial = deferred<DesktopSnapshot>();
		test.transport.snapshot.mockReturnValue(initial.promise);
		await Promise.resolve();
		test.feed.dispose();
		initial.resolve(snapshot(10));
		await test.feed.ready;
		test.send(11, { event: 'lifecycle', payload: { state: 'ready' } });
		await test.feed.refresh();
		test.feed.dispose();
		expect(test.stop).toHaveBeenCalledTimes(1);
		expect(test.onSnapshot).not.toHaveBeenCalled();
		expect(test.transport.snapshot).toHaveBeenCalledTimes(1);
	});

	it('cleans up bootstrap failure and reports failed recovery without accepting partial state', async () => {
		const failed = harness();
		failed.transport.snapshot.mockRejectedValue(new Error('Snapshot unavailable'));
		await expect(failed.feed.ready).rejects.toThrow('Snapshot unavailable');
		expect(failed.stop).toHaveBeenCalledTimes(1);
		const test = harness();
		await test.feed.ready;
		test.transport.snapshot.mockRejectedValueOnce(new Error('Recovery unavailable'));
		test.receive({ updates: [], resync_required: true });
		await vi.waitFor(() => expect(test.onError).toHaveBeenCalledTimes(1));
		test.transport.snapshot.mockResolvedValue(snapshot(2));
		test.send(3, { event: 'lifecycle', payload: { state: 'ready' } });
		await vi.waitFor(() => expect(test.onSnapshot.mock.lastCall?.[0].sequence).toBe(3));
		test.feed.dispose();
	});
});
