<script lang="ts">
	import type { WorkflowStatus } from './contracts/index';
	import { DesktopRequestError } from './desktop';
	import Icon from './Icon.svelte';
	import './theme.css';

	let {
		workflows,
		query,
		onQueryChange,
		onOpen,
		onRun,
		onCreate,
		disabled = false
	}: {
		workflows: readonly WorkflowStatus[];
		query?: string;
		onQueryChange?: (query: string) => void;
		onOpen: (id: string) => void;
		onRun: (id: string) => Promise<unknown>;
		onCreate: (name: string) => Promise<unknown>;
		disabled?: boolean;
	} = $props();
	let localQuery = $state('');
	let page = $state(0);
	let creating = $state(false);
	let workflowName = $state('');
	let createError = $state('');
	let creatingWorkflow = $state(false);
	let running = $state<string[]>([]);
	let runErrors = $state<Record<string, { revision: number | null; message: string }>>({});
	let observedSearch = $state('');
	let createDialog = $state<HTMLDialogElement>();
	let search = $derived(query ?? localQuery);
	let filtered = $derived.by(() => {
		const value = search.trim().toLocaleLowerCase();
		return value
			? workflows.filter((workflow) =>
					`${workflow.title} ${workflow.trigger_summary}`.toLocaleLowerCase().includes(value)
				)
			: workflows;
	});
	let pages = $derived(Math.max(1, Math.ceil(filtered.length / 50)));
	let visible = $derived(filtered.slice(page * 50, (page + 1) * 50));

	$effect(() => {
		if (search !== observedSearch) {
			observedSearch = search;
			page = 0;
		} else if (page >= pages) page = pages - 1;
	});

	$effect(() => {
		const revisions = new Map(workflows.map((workflow) => [workflow.id, workflow.revision]));
		const errors = Object.entries(runErrors);
		const current = errors.filter(
			([id, error]) => revisions.has(id) && revisions.get(id) === error.revision
		);
		if (current.length !== errors.length) runErrors = Object.fromEntries(current);
	});

	$effect(() => {
		if (!createDialog) return;
		if (creating && !createDialog.open) {
			if (typeof createDialog.showModal === 'function') createDialog.showModal();
			else createDialog.setAttribute('open', '');
			createDialog.querySelector('input')?.focus();
		} else if (!creating && createDialog.open) {
			if (typeof createDialog.close === 'function') createDialog.close();
			else createDialog.removeAttribute('open');
		}
	});

	function updateSearch(value: string) {
		if (query === undefined) localQuery = value;
		onQueryChange?.(value);
		page = 0;
	}

	function status(workflow: WorkflowStatus) {
		if (workflow.error !== null) return 'Unavailable';
		return workflow.enabled ? 'Enabled' : 'Disabled';
	}

	function canRun(workflow: WorkflowStatus) {
		return !disabled && workflow.enabled && workflow.error === null && workflow.has_steps;
	}

	function safeError(error: unknown, action: 'run' | 'create') {
		if (error instanceof DesktopRequestError) return error.message;
		return action === 'run'
			? 'This workflow could not be run. Try again.'
			: 'The workflow could not be created. Try again.';
	}

	async function run(workflow: WorkflowStatus) {
		if (!canRun(workflow) || running.includes(workflow.id)) return;
		const revision = workflow.revision;
		running = [...running, workflow.id];
		delete runErrors[workflow.id];
		try {
			await onRun(workflow.id);
		} catch (error) {
			const current = workflows.find((item) => item.id === workflow.id);
			if (current?.revision === revision && current.enabled && current.error === null)
				runErrors = { ...runErrors, [workflow.id]: { revision, message: safeError(error, 'run') } };
		} finally {
			running = running.filter((id) => id !== workflow.id);
		}
	}

	function openCreate() {
		if (disabled) return;
		creating = true;
		workflowName = '';
		createError = '';
	}

	function closeCreate() {
		if (creatingWorkflow) return;
		creating = false;
		createError = '';
	}

	async function create() {
		const name = workflowName.trim();
		if (!name || creatingWorkflow || disabled) return;
		creatingWorkflow = true;
		createError = '';
		try {
			await onCreate(name);
			creating = false;
			workflowName = '';
		} catch (error) {
			createError = safeError(error, 'create');
		} finally {
			creatingWorkflow = false;
		}
	}
</script>

<section class="workflow-editor library-view" aria-label="Workflow library">
	<div class="library-tools">
		<label class="search-field">
			<span class="visually-hidden">Search workflows</span>
			<input
				aria-label="Search workflows"
				placeholder="Search workflows"
				value={search}
				oninput={(event) => updateSearch(event.currentTarget.value)}
			/>
		</label>
		<button class="create-button" {disabled} aria-label="Create workflow" onclick={openCreate}
			><Icon name="plus" size={16} /><span>New workflow</span></button
		>
	</div>
	{#if !filtered.length}
		<p class="empty-state">{search ? 'No workflows match this search.' : 'No workflows yet.'}</p>
	{:else}
		<ul class="workflow-list" aria-label="Workflows">
			{#each visible as workflow (workflow.id)}
				<li class="workflow-row">
					<span class="workflow-icon"
						><Icon
							name={workflow.category === 'Chat'
								? 'message'
								: workflow.category === 'Broadcast'
									? 'monitor'
									: 'zap'}
							size={20}
						/></span
					>
					<div class="workflow-copy">
						<button
							class="workflow-name"
							aria-label={workflow.title}
							onclick={() => onOpen(workflow.id)}
						>
							<span>{workflow.title}</span>
							<span class="trigger-summary"
								>{workflow.trigger_summary} · {workflow.step_count}
								{workflow.step_count === 1 ? 'step' : 'steps'}</span
							>
						</button>
					</div>
					<span class="workflow-status" class:unavailable={workflow.error !== null}
						>{status(workflow)}</span
					>
					{#if canRun(workflow)}
						<button
							class="run-button"
							aria-label={`Run ${workflow.title}`}
							title={`Run ${workflow.title}`}
							disabled={running.includes(workflow.id)}
							onclick={() => run(workflow)}><Icon name="play" size={16} /></button
						>
					{:else}<span class="run-placeholder" aria-hidden="true"></span>{/if}
					{#if runErrors[workflow.id]}
						<div class="row-error" role="alert">
							<span>{runErrors[workflow.id].message}</span>
							<button
								aria-label={`Dismiss run error for ${workflow.title}`}
								onclick={() => {
									delete runErrors[workflow.id];
								}}><Icon name="close" size={14} /></button
							>
						</div>
					{/if}
				</li>
			{/each}
		</ul>
		{#if pages > 1}
			<nav class="pagination" aria-label="Workflow pages">
				<button aria-label="Previous page" disabled={page === 0} onclick={() => (page -= 1)}
					><Icon name="back" size={16} /></button
				>
				<span>Page {page + 1} of {pages}</span>
				<button aria-label="Next page" disabled={page + 1 >= pages} onclick={() => (page += 1)}
					><Icon name="back" size={16} /></button
				>
			</nav>
		{/if}
	{/if}

	{#if creating}
		<dialog
			bind:this={createDialog}
			aria-labelledby="create-title"
			oncancel={(event) => {
				event.preventDefault();
				closeCreate();
			}}
		>
			<form
				onsubmit={(event) => {
					event.preventDefault();
					void create();
				}}
			>
				<div class="dialog-heading">
					<h2 id="create-title">Create workflow</h2>
					<button
						type="button"
						class="icon-button"
						aria-label="Close"
						disabled={creatingWorkflow}
						onclick={closeCreate}><Icon name="close" size={18} /></button
					>
				</div>
				<label for="workflow-name">Name</label>
				<input id="workflow-name" bind:value={workflowName} required disabled={creatingWorkflow} />
				{#if createError}<div class="create-error" role="alert">
						<p>{createError}</p>
						<button
							type="button"
							aria-label="Dismiss create error"
							onclick={() => (createError = '')}><Icon name="close" size={14} /></button
						>
					</div>{/if}
				<div class="dialog-actions">
					<button type="button" disabled={creatingWorkflow} onclick={closeCreate}>Cancel</button>
					<button
						class="create-button"
						type="submit"
						disabled={creatingWorkflow || disabled || !workflowName.trim()}
						>{creatingWorkflow ? 'Creating…' : 'Create workflow'}</button
					>
				</div>
			</form>
		</dialog>
	{/if}
</section>

<style>
	.library-view {
		min-height: 100%;
		padding: 24px;
	}
	.library-tools {
		display: flex;
		align-items: center;
		gap: 12px;
		margin-bottom: 18px;
	}
	.search-field {
		flex: 1;
		min-width: 0;
	}
	.search-field input,
	dialog input {
		width: 100%;
		height: 36px;
		padding: 0 12px;
		background: var(--field);
		border: 1px solid var(--border);
		border-radius: 6px;
	}
	.create-button,
	.pagination button,
	.dialog-actions button {
		min-height: 36px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 8px;
		padding: 0 12px;
		border: 1px solid var(--border);
		border-radius: 6px;
		background: var(--surface);
	}
	.create-button {
		color: var(--text);
		border-color: var(--maroon);
		background: var(--maroon);
	}
	.workflow-list {
		list-style: none;
		padding: 0;
		margin: 0;
		border-top: 1px solid var(--border);
	}
	.workflow-row {
		min-height: 66px;
		display: grid;
		grid-template-columns: 24px minmax(0, 1fr) 100px 36px;
		align-items: center;
		column-gap: 16px;
		padding: 10px 8px 10px 12px;
		border-bottom: 1px solid var(--border);
		position: relative;
	}
	.workflow-copy {
		min-width: 0;
	}
	.workflow-icon {
		display: flex;
		align-items: center;
		color: var(--gold);
	}
	.workflow-name {
		display: block;
		width: 100%;
		max-width: 100%;
		padding: 0;
		border: 0;
		background: transparent;
		color: var(--text);
		text-align: left;
		font-size: 15px;
		font-weight: 600;
		overflow-wrap: anywhere;
	}
	.workflow-name:hover {
		color: var(--gold);
	}
	.trigger-summary {
		display: block;
		margin: 4px 0 0;
		color: var(--muted);
		font-size: 12px;
		font-weight: 400;
	}
	.workflow-status {
		color: var(--muted);
		font-size: 12px;
	}
	.workflow-status.unavailable {
		color: var(--gold);
	}
	.run-button,
	.icon-button {
		display: grid;
		place-items: center;
		width: 34px;
		height: 34px;
		padding: 0;
		border: 1px solid var(--border);
		border-radius: 6px;
		color: var(--gold);
		background: var(--surface);
	}
	.run-placeholder {
		width: 34px;
	}
	.row-error {
		grid-column: 1 / -1;
		display: flex;
		gap: 8px;
		align-items: center;
		margin-top: 8px;
		overflow-wrap: anywhere;
		color: var(--gold);
		font-size: 12px;
		line-height: 1.5;
	}
	.row-error span {
		flex: 1;
		min-width: 0;
	}
	.row-error button {
		border: 0;
		padding: 2px;
		color: inherit;
		background: transparent;
	}
	.empty-state {
		color: var(--muted);
		padding: 16px 12px;
	}
	.pagination {
		display: flex;
		justify-content: flex-end;
		align-items: center;
		gap: 12px;
		margin-top: 16px;
		color: var(--muted);
		font-size: 12px;
	}
	.pagination button:last-child :global(.icon) {
		transform: rotate(180deg);
	}
	dialog {
		width: min(440px, calc(100vw - 32px));
		padding: 22px;
		color: var(--text);
		background: var(--side);
		border: 1px solid var(--border);
		border-radius: 8px;
	}
	dialog::backdrop {
		background: rgb(0 0 0 / 65%);
	}
	dialog form {
		display: grid;
		gap: 12px;
	}
	.dialog-heading {
		display: flex;
		align-items: center;
		justify-content: space-between;
	}
	dialog h2 {
		margin: 0;
		font-size: 17px;
	}
	dialog label {
		color: var(--muted);
		font-size: 12px;
	}
	.create-error p {
		margin: 0;
		color: var(--gold);
		font-size: 12px;
	}
	.dialog-actions {
		display: flex;
		justify-content: flex-end;
		gap: 8px;
		margin-top: 8px;
	}
	.dialog-actions .create-button {
		background: var(--maroon);
		border-color: var(--maroon);
	}
	.create-error {
		display: flex;
		align-items: center;
		gap: 8px;
		color: var(--gold);
		font-size: 12px;
	}
	.create-error p {
		flex: 1;
		margin: 0;
	}
	.create-error button {
		display: grid;
		place-items: center;
		padding: 2px;
		border: 0;
		color: inherit;
		background: transparent;
	}
	.visually-hidden {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		margin: -1px;
		overflow: hidden;
		clip: rect(0, 0, 0, 0);
		white-space: nowrap;
		border: 0;
	}
	@media (max-width: 680px) {
		.library-view {
			padding: 16px;
		}
		.workflow-row {
			grid-template-columns: 24px minmax(0, 1fr) 82px 34px;
			gap: 8px;
		}
		.workflow-status {
			font-size: 11px;
		}
	}
</style>
