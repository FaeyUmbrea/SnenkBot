<script lang="ts">
	import { untrack } from 'svelte';
	import type { Snippet } from 'svelte';
	import type { ConnectionStatus, WorkflowStatus } from './contracts/index';
	import './theme.css';

	let {
		integration,
		connections,
		workflows,
		onOpen,
		children
	}: {
		integration: string;
		connections: readonly ConnectionStatus[];
		workflows: readonly WorkflowStatus[];
		onOpen: (workflowId: string) => void;
		children?: Snippet;
	} = $props();

	let page = $state(0);
	let observedIntegration = untrack(() => integration);
	const matchingConnections = $derived(
		connections.filter((item) => item.integration === integration)
	);
	const references = $derived(
		workflows.filter((workflow) => (workflow.integration_usage[integration]?.length ?? 0) > 0)
	);
	const pages = $derived(Math.max(1, Math.ceil(references.length / 50)));
	const visible = $derived(references.slice(page * 50, (page + 1) * 50));

	$effect(() => {
		const currentIntegration = integration;
		const currentPages = pages;
		if (currentIntegration !== observedIntegration) {
			observedIntegration = currentIntegration;
			page = 0;
		} else if (page >= currentPages) {
			page = currentPages - 1;
		}
	});

	function status(workflow: WorkflowStatus) {
		if (workflow.error !== null) return 'Unavailable';
		return workflow.enabled ? 'Enabled' : 'Disabled';
	}

	const connectionLabels = {
		inactive: 'Inactive',
		connecting: 'Connecting',
		connected: 'Connected',
		error: 'Connection error'
	};
</script>

<section class="workflow-editor integration-overview" aria-label="Integration overview">
	<section class="connections" aria-label="Connection">
		<h2>Connection</h2>
		{#if matchingConnections.length === 0}
			<p class="empty-state">No connection configured.</p>
		{:else}
			<ul aria-label="Connections">
				{#each matchingConnections as connection (`${connection.integration}:${connection.connection}`)}
					<li class="connection-row">
						<span
							class="pip"
							class:connected={connection.state === 'connected'}
							class:connecting={connection.state === 'connecting'}
							class:error={connection.state === 'error'}
							aria-hidden="true"
						></span>
						<div class="connection-copy">
							<strong>{connection.title}</strong>
							<span class="state-label">{connectionLabels[connection.state]}</span>
							{#if connection.detail}<span class="detail">{connection.detail}</span>{/if}
						</div>
					</li>
				{/each}
			</ul>
		{/if}
	</section>
	{#if children}<div class="integration-content">{@render children()}</div>{/if}

	<section class="usage" aria-label="Used by">
		<h2>Used by</h2>
		{#if references.length === 0}
			<p class="empty-state">No automations use this integration yet.</p>
		{:else}
			<ul aria-label="Automations using this integration">
				{#each visible as workflow (workflow.id)}
					<li class="usage-row">
						<button class="workflow-title" type="button" onclick={() => onOpen(workflow.id)}
							>{workflow.title}</button
						>
						<span class="features">{workflow.integration_usage[integration].join(' · ')}</span>
						<span class="workflow-state" class:unavailable={workflow.error !== null}
							>{status(workflow)}</span
						>
					</li>
				{/each}
			</ul>
			{#if pages > 1}
				<nav class="pagination" aria-label="Automation pages">
					<button
						type="button"
						aria-label="Previous page"
						disabled={page === 0}
						onclick={() => (page -= 1)}>Previous</button
					>
					<span>Page {page + 1} of {pages}</span>
					<button
						type="button"
						aria-label="Next page"
						disabled={page + 1 >= pages}
						onclick={() => (page += 1)}>Next</button
					>
				</nav>
			{/if}
		{/if}
	</section>
</section>

<style>
	.integration-overview {
		width: 100%;
		max-width: 920px;
		display: grid;
		gap: 32px;
	}
	h2 {
		margin: 0 0 12px;
		font-size: 16px;
		font-weight: 600;
	}
	ul {
		margin: 0;
		padding: 0;
		list-style: none;
	}
	li:last-child {
		border-bottom: 0;
	}
	.connection-row,
	.usage-row {
		min-height: 48px;
		padding: 10px 16px;
		background: var(--side);
		border-bottom: 1px solid var(--border);
	}
	.connection-row {
		display: flex;
		align-items: flex-start;
		gap: 10px;
	}
	.connection-copy {
		min-width: 0;
		display: flex;
		align-items: baseline;
		flex-wrap: wrap;
		gap: 4px 12px;
	}
	.connection-copy strong,
	.workflow-title,
	.features,
	.detail {
		overflow-wrap: anywhere;
		word-break: break-word;
	}
	.state-label,
	.detail,
	.features,
	.workflow-state,
	.empty-state,
	.pagination {
		color: var(--muted);
		font-size: 12px;
		line-height: 1.5;
	}
	.detail {
		flex-basis: 100%;
	}
	.pip {
		width: 8px;
		height: 8px;
		margin-top: 5px;
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
	.usage-row {
		display: grid;
		grid-template-columns: minmax(0, 1fr) minmax(0, 1fr) auto;
		align-items: center;
		gap: 12px;
	}
	.workflow-title {
		min-width: 0;
		padding: 0;
		border: 0;
		background: transparent;
		text-align: left;
		cursor: pointer;
	}
	.workflow-state {
		white-space: nowrap;
	}
	.workflow-state.unavailable {
		color: #e26c77;
	}
	.empty-state {
		margin: 0;
	}
	.pagination {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 12px;
		margin-top: 12px;
	}
	.pagination button {
		min-height: 32px;
		padding: 0 12px;
		border: 1px solid var(--border);
		border-radius: 4px;
		background: var(--side);
	}
	.pagination button:disabled {
		opacity: 0.5;
	}
	@media (max-width: 640px) {
		.usage-row {
			grid-template-columns: minmax(0, 1fr) auto;
		}
		.features {
			grid-column: 1;
		}
		.workflow-state {
			grid-column: 2;
			grid-row: 1 / span 2;
		}
	}
</style>
