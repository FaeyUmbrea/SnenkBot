<script lang="ts">
	import { onMount, untrack } from 'svelte';
	import type {
		ConfigSchema,
		HistoryFilter,
		HistoryPage,
		HistoryQuery,
		InspectHistory,
		RunRecord_Serialize,
		StepTrace
	} from './contracts/index';
	import { DesktopRequestError } from './desktop';
	import {
		formatDuration,
		runResult,
		runOutcomeLabels,
		schemaTitleIndex,
		traceTitle,
		traceFailure
	} from './history-format';
	import Icon from './Icon.svelte';
	import './theme.css';

	let {
		loadPage,
		inspectRun,
		workflowTitles = {},
		schemas = [],
		requestedRunId = null,
		onSelectRun
	}: {
		loadPage: (query: HistoryQuery) => Promise<HistoryPage>;
		inspectRun: (request: InspectHistory) => Promise<RunRecord_Serialize>;
		workflowTitles?: Readonly<Record<string, string>>;
		schemas?: readonly ConfigSchema[];
		requestedRunId?: string | null;
		onSelectRun?: (runId: string | null) => void;
	} = $props();
	let page = $state<HistoryPage | null>(null);
	let filter = $state<HistoryFilter>({ workflow_id: null, outcome: null, trigger_kind: null });
	let cursor = $state<string | null>(null);
	let previous = $state<(string | null)[]>([]);
	let loading = $state(false);
	let error = $state('');
	let detail = $state<RunRecord_Serialize | null>(null);
	let selectedId = $state<string | null>(null);
	let inspecting = $state(false);
	let inspectionError = $state('');
	let tracePage = $state(0);
	let loadGeneration = 0;
	let detailGeneration = 0;
	let disposed = false;
	const schemaTitles = $derived(schemaTitleIndex(schemas));
	const traces = $derived((detail?.step_trace ?? []).slice(tracePage * 50, (tracePage + 1) * 50));
	const dateTime = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' });
	const triggerLabels = {
		manual: 'Manual run',
		schedule: 'Schedule',
		event: 'Event',
		webhook: 'Webhook'
	};
	function timestamp(value: number) {
		const date = new Date(value);
		return Number.isFinite(date.getTime()) ? date : null;
	}
	function dateLabel(value: number) {
		const date = timestamp(value);
		return date ? dateTime.format(date) : 'Unknown time';
	}

	async function fetchPage(
		target: string | null = null,
		direction: 'refresh' | 'next' | 'previous' = 'refresh'
	) {
		const generation = ++loadGeneration;
		const oldCursor = cursor;
		const oldPrevious = [...previous];
		loading = true;
		error = '';
		try {
			const result = await loadPage({ page_size: 50, cursor: target, filter: { ...filter } });
			if (disposed || generation !== loadGeneration) return;
			page = result;
			cursor = target;
			// Cursor history is bounded independently of journal size; Refresh returns to newest runs.
			previous =
				direction === 'next'
					? [...oldPrevious, oldCursor].slice(-64)
					: direction === 'previous'
						? oldPrevious.slice(0, -1)
						: [];
		} catch (failure) {
			if (disposed || generation !== loadGeneration) return;
			error =
				failure instanceof DesktopRequestError
					? failure.message
					: 'Run history could not be loaded. Try refreshing.';
		} finally {
			if (!disposed && generation === loadGeneration) loading = false;
		}
	}

	function updateOutcome(event: Event) {
		const value = (event.currentTarget as HTMLSelectElement).value;
		filter = { ...filter, outcome: value === 'failed' ? 'failed' : null };
		page = null;
		cursor = null;
		previous = [];
		void fetchPage();
	}

	async function inspect(id: string) {
		const generation = ++detailGeneration;
		selectedId = id;
		detail = null;
		inspecting = true;
		inspectionError = '';
		tracePage = 0;
		try {
			const result = await inspectRun({ run_id: id });
			if (!disposed && generation === detailGeneration) {
				if (result.run_id === id) detail = result;
				else inspectionError = 'This run record could not be read.';
			}
		} catch (failure) {
			if (!disposed && generation === detailGeneration)
				inspectionError =
					failure instanceof DesktopRequestError
						? failure.message
						: 'This run could not be inspected. Try again.';
		} finally {
			if (!disposed && generation === detailGeneration) inspecting = false;
		}
	}

	function closeDetail() {
		++detailGeneration;
		selectedId = null;
		detail = null;
		inspecting = false;
		inspectionError = '';
	}
	function title(id: string) {
		return workflowTitles[id] ?? 'Unavailable workflow';
	}
	function failed(trace: StepTrace) {
		return trace.outcome.status === 'failed';
	}

	$effect(() => {
		const runId = requestedRunId;
		untrack(() => {
			if (disposed || runId === selectedId) return;
			if (runId === null) closeDetail();
			else void inspect(runId);
		});
	});
	function chooseRun(runId: string) {
		onSelectRun?.(runId);
		void inspect(runId);
	}
	function dismissDetail() {
		onSelectRun?.(null);
		closeDetail();
	}

	onMount(() => {
		void fetchPage();
		return () => {
			disposed = true;
			++loadGeneration;
			++detailGeneration;
		};
	});
</script>

<div class="workflow-editor history-view">
	<section class="run-list" aria-label="Run history" aria-busy={loading}>
		<div class="filters">
			<label
				>Results <select
					aria-label="Filter run results"
					value={filter.outcome ?? ''}
					onchange={updateOutcome}
					><option value="">All runs</option><option value="failed">Failed runs</option></select
				></label
			>
			<button class="refresh" disabled={loading} onclick={() => fetchPage()}
				><Icon name="history" size={16} />Refresh</button
			>
		</div>
		{#if error}<p role="alert">{error}</p>{/if}
		{#if loading && !page}<p role="status">Loading runs…</p>{/if}
		{#if page}
			<div class="column-heading"><span>Automation</span><span>Result / time</span></div>
			<ul class="runs">
				{#each page.entries as entry (entry.kind === 'record' ? entry.summary.run_id : entry.file_name)}
					<li>
						{#if entry.kind === 'record'}
							{@const run = entry.summary}
							<button
								class:selected={selectedId === run.run_id}
								onclick={() => chooseRun(run.run_id)}
								aria-label={`Inspect ${title(run.workflow_id)} run`}
							>
								<span class="workflow-title">{title(run.workflow_id)}</span>
								<span class="result" class:failure={run.outcome.status === 'failed'}
									>{runResult(run.outcome)}{#if run.finished_at_ms !== null}
										· {formatDuration(Math.max(0, run.finished_at_ms - run.started_at_ms))}{/if} ·
									<time datetime={timestamp(run.started_at_ms)?.toISOString()}
										>{dateLabel(run.started_at_ms)}</time
									></span
								>
							</button>
						{:else}
							<div class="corrupt" role="status">
								<span>Unreadable run</span><span>{entry.message}</span>
							</div>
						{/if}
					</li>
				{/each}
			</ul>
			{#if !page.entries.length}<p>No runs match this view.</p>{/if}
			<div class="pagination">
				<button
					disabled={loading || !previous.length}
					onclick={() => fetchPage(previous.at(-1) ?? null, 'previous')}
					><Icon name="back" size={16} />Newer</button
				><button
					disabled={loading || !page.next_cursor}
					onclick={() => fetchPage(page?.next_cursor ?? null, 'next')}
					>Older<Icon name="down" size={16} /></button
				>
			</div>
		{/if}
	</section>
	{#if selectedId !== null}
		<aside class="run-detail" aria-label="Run details" aria-busy={inspecting}>
			<div class="detail-heading">
				<strong>Run details</strong><button aria-label="Close run details" onclick={dismissDetail}
					><Icon name="close" size={16} /></button
				>
			</div>
			{#if inspecting}<p role="status">Loading run…</p>{/if}
			{#if inspectionError}<p role="alert">{inspectionError}</p>
				<button onclick={() => inspect(selectedId!)}>Try again</button>{/if}
			{#if detail}
				<h2>{title(detail.workflow_id)}</h2>
				<p class:failure={detail.outcome.status === 'failed'}>{runResult(detail.outcome)}</p>
				<dl>
					<div>
						<dt>Started</dt>
						<dd>{dateLabel(detail.started_at_ms)}</dd>
					</div>
					<div>
						<dt>Activation</dt>
						<dd>{triggerLabels[detail.trigger.kind]}</dd>
					</div>
					<div>
						<dt>Saved version</dt>
						<dd>{detail.workflow_revision}</dd>
					</div>
					{#if detail.finished_at_ms !== null}<div>
							<dt>Duration</dt>
							<dd>{formatDuration(Math.max(0, detail.finished_at_ms - detail.started_at_ms))}</dd>
						</div>{/if}
				</dl>
				<h3>Steps</h3>
				{#if !detail.step_trace?.length}<p>No step timings were recorded for this run.</p>{/if}
				<ol class="trace-list" start={tracePage * 50 + 1}>
					{#each traces as trace, traceIndex (traceIndex)}
						<li class:failure={failed(trace)}>
							<strong>{traceTitle(trace, schemaTitles, workflowTitles)}</strong><span
								>{runOutcomeLabels[trace.outcome.status]} · {formatDuration(
									trace.duration_ms
								)}</span
							>{#if traceFailure(trace)}<p>{traceFailure(trace)}</p>{/if}
						</li>
					{/each}
				</ol>
				{#if (detail.step_trace?.length ?? 0) > 50}<div class="pagination">
						<button disabled={tracePage === 0} onclick={() => (tracePage -= 1)}
							>Previous steps</button
						><button
							disabled={(tracePage + 1) * 50 >= (detail.step_trace?.length ?? 0)}
							onclick={() => (tracePage += 1)}>Next steps</button
						>
					</div>{/if}
			{/if}
		</aside>
	{/if}
</div>

<style>
	.history-view {
		display: flex;
		height: 100%;
		min-height: 0;
	}
	.run-list {
		flex: 1;
		min-width: 0;
		max-width: 1180px;
		padding: 26px 24px 24px;
		overflow: auto;
	}
	.filters {
		display: flex;
		align-items: center;
		gap: 16px;
		margin-bottom: 28px;
	}
	.filters label {
		display: flex;
		align-items: center;
		gap: 8px;
		color: var(--muted);
		font-size: 12px;
	}
	select,
	button {
		min-height: 32px;
		background: var(--surface);
		border: 1px solid transparent;
		border-radius: 6px;
		padding: 6px 10px;
	}
	button {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		justify-content: center;
	}
	.refresh {
		margin-left: auto;
	}
	.column-heading,
	.runs button,
	.corrupt {
		display: grid;
		grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
		gap: 16px;
		padding: 14px 16px;
	}
	.column-heading {
		min-height: 48px;
		align-items: center;
		background: var(--side);
		color: var(--muted);
		font-size: 14px;
	}
	.runs {
		list-style: none;
		margin: 8px 0 0;
		padding: 0;
	}
	.runs li {
		margin-bottom: 8px;
	}
	.runs button {
		width: 100%;
		min-height: 48px;
		background: var(--side);
		text-align: left;
		border-radius: 0;
		align-items: center;
	}
	.runs button.selected {
		border-color: var(--amber);
	}
	.workflow-title,
	.result {
		overflow-wrap: anywhere;
	}
	.result,
	.corrupt {
		color: var(--muted);
		font-size: 14px;
	}
	time {
		display: inline;
		color: var(--muted);
	}
	.corrupt {
		border: 1px solid var(--border);
	}
	.pagination {
		display: flex;
		justify-content: flex-end;
		gap: 8px;
		margin-top: 16px;
	}
	.run-detail {
		flex: none;
		width: 384px;
		padding: 24px;
		background: var(--side);
		border-left: 1px solid var(--border);
		overflow: auto;
	}
	.detail-heading {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 16px;
	}
	h2 {
		font-size: 16px;
		margin: 24px 0 12px;
		overflow-wrap: anywhere;
	}
	h3 {
		font-size: 14px;
		margin: 24px 0 12px;
	}
	p {
		font-size: 12px;
		line-height: 1.5;
		color: var(--muted);
		overflow-wrap: anywhere;
	}
	dl {
		font-size: 12px;
	}
	dl div {
		display: flex;
		justify-content: space-between;
		gap: 16px;
		margin-top: 12px;
	}
	dd {
		margin: 0;
		text-align: right;
	}
	dt {
		color: var(--muted);
	}
	.trace-list {
		padding-left: 24px;
	}
	.trace-list li {
		margin-top: 12px;
		padding: 8px;
		border-bottom: 1px solid var(--border);
	}
	.trace-list li:last-child {
		border: 0;
	}
	.trace-list strong,
	.trace-list span {
		display: block;
		font-size: 12px;
		overflow-wrap: anywhere;
	}
	.trace-list span {
		margin-top: 4px;
		color: var(--muted);
	}
	.failure,
	p[role='alert'] {
		color: var(--gold);
	}
	@media (max-width: 1100px) {
		.run-list {
			padding: 26px 16px 24px;
		}
		.run-detail {
			padding: 24px 16px;
		}
	}
</style>
