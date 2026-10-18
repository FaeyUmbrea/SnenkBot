<script lang="ts">
	import { onMount } from 'svelte';
	import { DesktopRequestError } from './desktop';
	import type {
		EventPage,
		EventIdentity,
		EventReplayTarget,
		EventTargetPage,
		ReplayEvent,
		EventReplayResult
	} from './contracts';

	let {
		load,
		targets,
		replay,
		active = true
	}: {
		load: () => Promise<EventPage>;
		targets: (request: EventIdentity) => Promise<EventTargetPage>;
		replay: (request: ReplayEvent) => Promise<EventReplayResult>;
		active?: boolean;
	} = $props();
	let page = $state<EventPage | null>(null);
	let error = $state('');
	let message = $state('');
	let loading = $state(false);
	let replaying = $state(false);
	let eventId = $state<string | null>(null);
	let destinations = $state<EventReplayTarget[]>([]);
	let loadingTargets = $state(false);
	let selected = $state<string[]>([]);
	let dialog: HTMLDialogElement;
	let mounted = true;
	let generation = 0;
	const dateTime = new Intl.DateTimeFormat(undefined, { dateStyle: 'short', timeStyle: 'medium' });
	/** Format valid stored timestamps without allowing a corrupt date to break the list. */
	function timestamp(value: number): string {
		const date = new Date(value);
		return Number.isFinite(date.getTime()) ? dateTime.format(date) : 'Unknown time';
	}
	/** Refresh the bounded event list without interrupting a replay or open selector. */
	async function refresh() {
		if (loading) return;
		loading = true;
		try {
			const result = await load();
			if (mounted) page = result;
		} catch (cause) {
			if (mounted)
				error =
					cause instanceof DesktopRequestError ? cause.message : 'Events could not be loaded.';
		} finally {
			if (mounted) loading = false;
		}
	}
	onMount(() => {
		void refresh();
		const timer = setInterval(() => {
			if (active) void refresh();
		}, 3000);
		return () => {
			mounted = false;
			generation++;
			clearInterval(timer);
		};
	});
	/** Open an explicit recipient selection for one recorded event. */
	async function choose(id: string) {
		if (replaying) return;
		const attempt = ++generation;
		eventId = id;
		loadingTargets = true;
		selected = [];
		destinations = [];
		error = '';
		message = '';
		dialog.showModal();
		try {
			const result = await targets({ event_id: id });
			if (mounted && attempt === generation) {
				destinations = result.targets;
				if (result.errors.length) error = result.errors.join(' ');
			}
		} catch (cause) {
			if (mounted && attempt === generation)
				error =
					cause instanceof DesktopRequestError
						? cause.message
						: 'Replay recipients could not be loaded.';
		} finally {
			if (mounted && attempt === generation) loadingTargets = false;
		}
	}
	/** Close the selection and invalidate any in-flight destination lookup. */
	function cancel() {
		generation++;
		eventId = null;
		dialog.close();
	}
	/** Replay once through normal dispatch or only the explicitly selected recipients. */
	async function run(id: string, mode: 'normal' | 'selected') {
		if (replaying || (mode === 'selected' && selected.length === 0)) return;
		replaying = true;
		error = '';
		message = '';
		try {
			const result = await replay({
				event_id: id,
				targets: mode === 'selected' ? [...selected] : null
			});
			if (!mounted) return;
			message =
				mode === 'normal'
					? 'Event replayed through its normal dispatch path.'
					: `Replay dispatched to ${result.workflows_started} automations and ${result.consumers_delivered.length} consumers.`;
			if (result.warnings.length) error = result.warnings.join(' ');
			if (mode === 'selected') cancel();
		} catch (cause) {
			if (mounted)
				error =
					cause instanceof DesktopRequestError ? cause.message : 'The event could not be replayed.';
		} finally {
			if (mounted) replaying = false;
		}
	}
</script>

<section aria-label="Events" aria-busy={loading}>
	<header>
		<div>
			<h1>Events</h1>
			<p>Received events, excluding chat. Replay follows the current configuration.</p>
		</div>
		<button
			disabled={loading}
			onclick={() => {
				error = '';
				void refresh();
			}}>Refresh</button
		>
	</header>
	{#if error && !eventId}<p role="alert">{error}</p>{/if}
	{#if message}<p role="status">{message}</p>{/if}
	{#each page?.errors ?? [] as problem, index (index)}<p role="alert">{problem}</p>{/each}
	{#if page && !page.events.length}<p>No events recorded yet.</p>{/if}
	{#each page?.events ?? [] as event (event.id)}
		<article>
			<div>
				<strong>{event.name}</strong>
				<p>{event.source} · {timestamp(event.received_at_ms)}</p>
			</div>
			<button disabled={replaying || !event.replayable} onclick={() => run(event.id, 'normal')}
				>Replay</button
			><button disabled={replaying || !event.replayable} onclick={() => choose(event.id)}
				>Replay to…</button
			>{#if !event.replayable}<span>Replay unavailable</span>{/if}
		</article>
	{/each}
</section>
<dialog
	bind:this={dialog}
	aria-labelledby="replay-title"
	oncancel={(event) => {
		event.preventDefault();
		if (!replaying) cancel();
	}}
>
	<h2 id="replay-title">Replay to…</h2>
	<p>Send this event only to the selected consumers and automations.</p>
	{#each [{ kind: 'consumer', label: 'Consumers' }, { kind: 'workflow', label: 'Automations' }] as group (group.kind)}
		{#if destinations.some((destination) => destination.kind === group.kind)}
			<fieldset>
				<legend>{group.label}</legend>
				{#each destinations.filter((destination) => destination.kind === group.kind) as destination (destination.id)}
					<label
						><input
							type="checkbox"
							bind:group={selected}
							value={destination.id}
							disabled={replaying}
						/>{destination.label}</label
					>
				{/each}
			</fieldset>
		{/if}
	{/each}
	{#if loadingTargets}<p>Loading recipients…</p>{:else if !destinations.length}<p>
			No applicable recipients are available.
		</p>{/if}
	{#if error}<p role="alert">{error}</p>{/if}
	<footer>
		<button disabled={replaying} onclick={cancel}>Cancel</button><button
			disabled={replaying || !selected.length}
			onclick={() => {
				if (eventId) void run(eventId, 'selected');
			}}>Replay selected</button
		>
	</footer>
</dialog>

<style>
	section {
		width: 100%;
	}
	header,
	article,
	footer {
		display: flex;
		align-items: center;
		gap: 12px;
	}
	header > div,
	article > div {
		flex: 1;
	}
	h1,
	h2 {
		font-size: 18px;
		font-weight: 600;
	}
	p {
		color: var(--muted);
	}
	article {
		padding: 12px 0;
		border-bottom: 1px solid var(--border);
	}
	article p {
		margin: 4px 0;
		font-size: 12px;
	}
	button {
		min-height: 32px;
		border: 1px solid var(--border);
		border-radius: 4px;
		background: var(--surface);
		padding: 4px 12px;
	}
	dialog {
		background: var(--side);
		color: var(--text);
		border: 1px solid var(--border);
		border-radius: 6px;
		max-width: 480px;
	}
	dialog::backdrop {
		background: #0009;
	}
	label {
		display: flex;
		gap: 8px;
		padding: 8px 0;
	}
	footer {
		justify-content: flex-end;
		margin-top: 16px;
	}
</style>
