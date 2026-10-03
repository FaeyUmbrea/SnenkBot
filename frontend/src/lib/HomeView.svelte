<script lang="ts">
	import type {
		ConnectionStatus,
		HistoryPage,
		RunSummary,
		WorkflowStatus
	} from './contracts/index';
	import { DesktopRequestError } from './desktop';
	import { runResult } from './history-format';
	import { type DesktopPage } from './navigation';
	import Icon from './Icon.svelte';
	import './theme.css';

	let {
		connections,
		workflows,
		history,
		historyLoading = false,
		historyError = '',
		onConfigure,
		onNavigate,
		onOpen,
		onRun,
		onLibrary,
		onHistory,
		onRefreshHistory
	}: {
		connections: readonly ConnectionStatus[];
		workflows: readonly WorkflowStatus[];
		history: HistoryPage | null;
		historyLoading?: boolean;
		historyError?: string;
		onConfigure: (connection: ConnectionStatus) => void;
		onNavigate: (page: DesktopPage) => void;
		onOpen: (workflowId: string) => void;
		onRun: (workflowId: string) => Promise<unknown>;
		onLibrary: () => void;
		onHistory: (runId?: string) => void;
		onRefreshHistory: () => Promise<void> | void;
	} = $props();
	let pendingRuns = $state<string[]>([]);
	let runErrors = $state<Record<string, { revision: number | null; message: string }>>({});
	const visibleWorkflows = $derived(workflows.slice(0, 6));
	const recentRuns = $derived((history?.entries ?? []).slice(0, 6));
	const stateLabels = {
		connected: 'Connected',
		connecting: 'Connecting',
		inactive: 'Inactive',
		error: 'Connection error'
	};
	const categoryIcons = { Chat: 'message', Broadcast: 'monitor', Automation: 'zap' } as const;
	const dateTime = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' });

	function canRun(workflow: WorkflowStatus) {
		return workflow.enabled && workflow.error === null && workflow.has_steps;
	}

	function safeDate(value: number) {
		const date = new Date(value);
		return Number.isFinite(date.getTime()) ? date : null;
	}

	function dateLabel(value: number) {
		const date = safeDate(value);
		return date ? dateTime.format(date) : 'Unknown time';
	}

	function runTitle(run: RunSummary) {
		return (
			workflows.find((workflow) => workflow.id === run.workflow_id)?.title ?? 'Unavailable workflow'
		);
	}

	function dismissRunError(id: string) {
		const remaining = { ...runErrors };
		delete remaining[id];
		runErrors = remaining;
	}

	async function run(workflow: WorkflowStatus) {
		if (!canRun(workflow) || pendingRuns.includes(workflow.id)) return;
		const identity = { id: workflow.id, revision: workflow.revision };
		pendingRuns = [...pendingRuns, workflow.id];
		dismissRunError(workflow.id);
		try {
			await onRun(workflow.id);
		} catch (error) {
			const current = workflows.find((item) => item.id === identity.id);
			if (current?.revision === identity.revision) {
				runErrors = {
					...runErrors,
					[workflow.id]: {
						revision: identity.revision,
						message:
							error instanceof DesktopRequestError
								? error.message
								: 'This automation could not be started. Try again.'
					}
				};
			}
		} finally {
			pendingRuns = pendingRuns.filter((id) => id !== workflow.id);
		}
	}

	$effect(() => {
		const current = new Map(workflows.map((workflow) => [workflow.id, workflow.revision]));
		const next = Object.fromEntries(
			Object.entries(runErrors).filter(([id, error]) => current.get(id) === error.revision)
		);
		if (Object.keys(next).length !== Object.keys(runErrors).length) runErrors = next;
	});
</script>

<div class="workflow-editor home-view">
	{#if connections.length}
		<section aria-labelledby="home-connections-heading">
			<h2 id="home-connections-heading">Connections</h2>
			<ul class="connection-list">
				{#each connections as connection (`${connection.integration}:${connection.connection}`)}
					<li>
						<div class="connection-name">
							<span class="pip {connection.state}"></span><strong>{connection.title}</strong>
						</div>
						<div class="connection-detail">
							<span>{stateLabels[connection.state]}</span>{#if connection.detail}<span
									>{connection.detail}</span
								>{/if}
						</div>
						<button class="configure" onclick={() => onConfigure(connection)}
							>Configure <Icon name="settings" size={15} /></button
						>
					</li>
				{/each}
			</ul>
		</section>
	{:else}
		<section class="empty-connections" aria-labelledby="home-connections-heading">
			<h2 id="home-connections-heading">Connections</h2>
			<p>No connections are configured yet.</p>
			<button class="text-action" onclick={() => onNavigate('streaming')}
				>Set up a connection</button
			>
		</section>
	{/if}

	<section class="automation-section" aria-labelledby="home-automations-heading">
		<div class="section-heading">
			<h2 id="home-automations-heading">Automations</h2>
			<button class="text-action" onclick={onLibrary}>View all</button>
		</div>
		{#if visibleWorkflows.length}
			<ul class="workflow-list" aria-label="Automations">
				{#each visibleWorkflows as workflow (workflow.id)}
					<li>
						<button
							class="workflow-open"
							onclick={() => onOpen(workflow.id)}
							aria-label={`Open ${workflow.title}`}
						>
							<span class="category-icon"
								><Icon name={categoryIcons[workflow.category]} size={18} /></span
							>
							<span class="workflow-copy"
								><strong>{workflow.title}</strong><span>{workflow.trigger_summary}</span></span
							>
							<span class="workflow-meta"
								>{workflow.enabled ? 'Enabled' : 'Disabled'}<span
									>{workflow.step_count} {workflow.step_count === 1 ? 'step' : 'steps'}</span
								></span
							>
						</button>
						<div class="workflow-actions" class:has-error={!!runErrors[workflow.id]}>
							{#if runErrors[workflow.id]?.revision === workflow.revision}
								<p role="alert">{runErrors[workflow.id].message}</p>
								<button
									class="dismiss-error"
									aria-label={`Dismiss error for ${workflow.title}`}
									onclick={() => dismissRunError(workflow.id)}
									><Icon name="close" size={14} /></button
								>
							{/if}
							<button
								class="run"
								aria-label={`Run ${workflow.title}`}
								disabled={!canRun(workflow) || pendingRuns.includes(workflow.id)}
								onclick={() => run(workflow)}
								><Icon
									name="play"
									size={16}
								/>{#if pendingRuns.includes(workflow.id)}Starting…{/if}</button
							>
						</div>
					</li>
				{/each}
			</ul>
		{:else}
			<p class="empty-copy">No automations yet.</p>
		{/if}
	</section>

	<section
		class="history-section"
		aria-labelledby="home-history-heading"
		aria-busy={historyLoading}
	>
		<div class="section-heading">
			<h2 id="home-history-heading">Recent runs</h2>
			<button class="text-action" onclick={() => onHistory()}>View history</button>
		</div>
		{#if historyError}
			<div class="history-error">
				<p role="alert">{historyError}</p>
				<button class="text-action" disabled={historyLoading} onclick={() => onRefreshHistory()}
					>{historyLoading ? 'Refreshing…' : 'Try again'}</button
				>
			</div>
		{/if}
		{#if historyLoading && history === null && !historyError}
			<p role="status" class="empty-copy">Loading runs…</p>
		{:else if recentRuns.length}
			<ul class="run-list" aria-label="Recent runs">
				{#each recentRuns as entry, index (entry.kind === 'record' ? entry.summary.run_id : `${entry.file_name}:${index}`)}
					<li>
						{#if entry.kind === 'record'}
							{@const runSummary = entry.summary}
							<button
								class="run-row"
								aria-label={`View ${runTitle(runSummary)} run`}
								onclick={() => onHistory(runSummary.run_id)}
							>
								<strong>{runTitle(runSummary)}</strong>
								<span class:failed={runSummary.outcome.status === 'failed'}
									>{runResult(runSummary.outcome)} · {dateLabel(runSummary.started_at_ms)}</span
								>
							</button>
						{:else}
							<button
								class="run-row corrupt"
								aria-label="View unavailable run record"
								onclick={() => onHistory()}
								><strong>Unavailable run record</strong><span
									>Open run history to review this record.</span
								></button
							>
						{/if}
					</li>
				{/each}
			</ul>
		{:else}
			<p class="empty-copy">No runs yet.</p>
		{/if}
	</section>
</div>

<style>
	.home-view {
		min-height: 100%;
		padding: 28px 24px 32px;
	}
	section {
		max-width: 1180px;
	}
	h2 {
		margin: 0;
		font-size: 16px;
		font-weight: 600;
	}
	.connection-list,
	.workflow-list,
	.run-list {
		list-style: none;
		margin: 18px 0 0;
		padding: 0;
	}
	.connection-list {
		display: grid;
		gap: 8px;
	}
	.connection-list li {
		display: grid;
		grid-template-columns: minmax(180px, 0.85fr) minmax(220px, 1.5fr) auto;
		align-items: center;
		gap: 18px;
		min-height: 54px;
		padding: 10px 16px;
		border-radius: 8px;
		background: var(--side);
	}
	.connection-name {
		display: flex;
		align-items: center;
		gap: 12px;
		min-width: 0;
	}
	.connection-name strong {
		overflow-wrap: anywhere;
	}
	.pip {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--muted);
		flex: none;
	}
	.pip.connected {
		background: #79bd8e;
	}
	.pip.connecting {
		background: var(--gold);
	}
	.pip.error {
		background: #e26c77;
	}
	.connection-detail {
		display: flex;
		flex-wrap: wrap;
		gap: 8px 14px;
		color: var(--muted);
		font-size: 13px;
	}
	.connection-detail span + span::before {
		content: '·';
		margin-right: 14px;
		color: var(--border);
	}
	.configure,
	.run,
	.dismiss-error {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 8px;
		min-height: 32px;
		padding: 6px 9px;
		border: 0;
		border-radius: 6px;
		background: var(--surface);
	}
	.configure {
		color: var(--text);
	}
	.automation-section {
		margin-top: 52px;
	}
	.history-section {
		margin-top: 44px;
	}
	.section-heading {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 16px;
	}
	.text-action {
		color: var(--gold);
		background: transparent;
		border: 0;
		padding: 6px 0;
	}
	.workflow-list {
		display: grid;
		gap: 7px;
	}
	.workflow-list li {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		align-items: center;
		gap: 10px;
		min-height: 62px;
		padding: 8px 12px;
		border-top: 2px solid var(--maroon);
		border-radius: 6px;
		background: var(--side);
	}
	.workflow-open {
		display: flex;
		align-items: center;
		gap: 14px;
		min-width: 0;
		text-align: left;
		background: transparent;
		border: 0;
		padding: 4px;
	}
	.category-icon {
		color: var(--gold);
	}
	.workflow-copy {
		display: grid;
		min-width: 0;
		gap: 4px;
	}
	.workflow-copy strong,
	.workflow-copy > span {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.workflow-copy > span,
	.workflow-meta {
		color: var(--muted);
		font-size: 12px;
	}
	.workflow-meta {
		display: flex;
		gap: 18px;
		margin-left: auto;
	}
	.workflow-actions {
		display: flex;
		align-items: center;
		gap: 8px;
	}
	.workflow-actions.has-error {
		grid-column: 1 / -1;
	}
	.workflow-actions.has-error .run {
		margin-left: auto;
	}
	.workflow-actions p {
		margin: 0;
		max-width: none;
		color: var(--gold);
		font-size: 12px;
	}
	.dismiss-error {
		width: 28px;
		min-height: 28px;
		padding: 4px;
	}
	.run {
		min-width: 34px;
		min-height: 32px;
	}
	.run-list {
		margin-top: 16px;
	}
	.run-list li + li {
		border-top: 1px solid var(--border);
	}
	.run-row {
		display: grid;
		grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
		align-items: center;
		gap: 16px;
		width: 100%;
		min-height: 48px;
		padding: 10px 16px;
		border: 0;
		background: var(--side);
		text-align: left;
	}
	.run-row span {
		color: var(--muted);
		font-size: 13px;
	}
	.run-row span.failed {
		color: var(--gold);
	}
	.corrupt strong {
		color: var(--muted);
		font-weight: 500;
	}
	.empty-connections {
		padding: 20px 0 4px;
	}
	.empty-connections p,
	.empty-copy,
	.history-error p {
		color: var(--muted);
		font-size: 13px;
	}
	.history-error {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 12px;
	}
	@media (max-width: 760px) {
		.home-view {
			padding: 20px 16px 24px;
		}
		.connection-list li {
			grid-template-columns: minmax(0, 1fr) auto;
			gap: 8px;
		}
		.connection-detail {
			grid-column: 1;
			padding-left: 20px;
		}
		.configure {
			grid-column: 2;
			grid-row: 1 / span 2;
		}
		.workflow-list li {
			grid-template-columns: minmax(0, 1fr) auto;
		}
		.workflow-open {
			gap: 9px;
		}
		.workflow-meta {
			display: none;
		}
		.workflow-actions p {
			max-width: none;
		}
		.run-row {
			grid-template-columns: minmax(0, 1fr);
			gap: 5px;
		}
	}
</style>
