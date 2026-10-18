import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { DesktopBatch, DesktopSnapshot, DesktopUpdate, Event } from './contracts/index';

const ERROR_CAPACITY = 32;
const UPDATE_CAPACITY = 256;

export interface DesktopFeedTransport {
	listen: (receive: (batch: DesktopBatch) => void) => Promise<UnlistenFn>;
	snapshot: () => Promise<DesktopSnapshot>;
}

export interface DesktopFeedCallbacks {
	onSnapshot: (snapshot: DesktopSnapshot) => void;
	/** Engine events have no run identity; consumers receive the original event unchanged. */
	onEngineEvent?: (event: Event) => void;
	onError?: (error: unknown) => void;
}

export interface DesktopFeed {
	ready: Promise<void>;
	refresh: () => Promise<void>;
	dispose: () => void;
}

const desktopTransport: DesktopFeedTransport = {
	listen: (receive) => listen<DesktopBatch>('desktop-update', (event) => receive(event.payload)),
	snapshot: () => invoke<DesktopSnapshot>('desktop_snapshot')
};

function applyUpdate(snapshot: DesktopSnapshot, update: DesktopUpdate): DesktopSnapshot {
	const next = { ...snapshot, sequence: update.sequence };
	const event = update.event;
	switch (event.event) {
		case 'lifecycle':
			next.startup = event.payload;
			break;
		case 'inventory':
			next.workflows = event.payload;
			break;
		case 'connections':
			next.connections = event.payload.connections;
			next.reconfiguration_requests = event.payload.requests;
			break;
		case 'twitch_authentication':
			next.twitch_authentication = event.payload;
			break;
		case 'run_completed':
			next.last_completed = event.payload;
			break;
		case 'input':
			if (event.payload.event !== 'closed') {
				next.pending_input = event.payload.payload;
			} else if (next.pending_input?.request_id === event.payload.payload.request_id) {
				next.pending_input = null;
			}
			break;
		case 'error_added':
			next.errors = [...snapshot.errors, event.payload].slice(-ERROR_CAPACITY);
			break;
		case 'error_dismissed':
			next.errors = snapshot.errors.filter((notice) => notice.id !== event.payload.id);
			break;
		case 'engine':
			break;
	}
	return next;
}

/** Subscribe before reading the snapshot so native updates cannot fall between the two. */
export function subscribeDesktopUpdates(
	callbacks: DesktopFeedCallbacks,
	transport: DesktopFeedTransport = desktopTransport
): DesktopFeed {
	let disposed = false;
	let unlisten: UnlistenFn | undefined;
	let snapshot: DesktopSnapshot | undefined;
	let refreshPromise: Promise<void> | undefined;
	let refreshing = false;
	let refreshPending = false;
	const buffered = new Map<number, DesktopUpdate>();

	function dispose() {
		disposed = true;
		buffered.clear();
		unlisten?.();
		unlisten = undefined;
	}

	function drainUpdates() {
		while (buffered.size && !disposed && !refreshPending) {
			const updates = [...buffered.values()].sort((left, right) => left.sequence - right.sequence);
			for (const update of updates) {
				if (disposed || refreshPending) return;
				if (!snapshot) return;
				if (update.sequence > snapshot.sequence + 1) {
					refreshPending = true;
					return;
				}
				buffered.delete(update.sequence);
				if (update.sequence <= snapshot.sequence) continue;
				snapshot = applyUpdate(snapshot, update);
				callbacks.onSnapshot(snapshot);
				if (!disposed && update.event.event === 'engine') {
					callbacks.onEngineEvent?.(update.event.payload);
				}
			}
		}
	}

	function refreshSnapshot(): Promise<void> {
		if (disposed) return Promise.resolve();
		if (refreshing) return refreshPromise ?? Promise.resolve();
		refreshing = true;
		refreshPromise = (async () => {
			try {
				do {
					refreshPending = false;
					const next = await transport.snapshot();
					if (disposed) return;
					// A delayed snapshot must never roll back state already delivered to the view.
					if (!snapshot || next.sequence >= snapshot.sequence) {
						snapshot = { ...next, errors: next.errors.slice(-ERROR_CAPACITY) };
						callbacks.onSnapshot(snapshot);
					}
					if (!refreshPending) drainUpdates();
				} while (refreshPending && !disposed);
			} catch (error) {
				refreshPending = true;
				throw error;
			} finally {
				refreshing = false;
				refreshPromise = undefined;
			}
		})();
		return refreshPromise;
	}

	function receive(batch: DesktopBatch) {
		if (disposed) return;
		if (batch.resync_required) refreshPending = true;
		for (const update of batch.updates) {
			if (snapshot && update.sequence <= snapshot.sequence) continue;
			buffered.set(update.sequence, update);
			if (buffered.size > UPDATE_CAPACITY) {
				// Replace an overflowing bootstrap buffer with a coherent native snapshot.
				buffered.clear();
				refreshPending = true;
			}
		}
		if (snapshot && !refreshing && !refreshPending) drainUpdates();
		if (refreshPending && unlisten && !refreshing) {
			void refreshSnapshot().catch((error: unknown) => {
				if (!disposed) callbacks.onError?.(error);
			});
		}
	}

	const ready = (async () => {
		try {
			const stop = await transport.listen(receive);
			if (disposed) {
				stop();
				return;
			}
			unlisten = stop;
			await refreshSnapshot();
		} catch (error) {
			dispose();
			throw error;
		}
	})();

	return {
		ready,
		refresh: () => refreshPromise ?? ready.then(refreshSnapshot),
		dispose
	};
}
