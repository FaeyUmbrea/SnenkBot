<script lang="ts">
	import type { Snippet } from 'svelte';
	import type {
		ConnectionStatus as Connection,
		ErrorNotice,
		ReconfigurationRequest
	} from './contracts/index';
	import { desktopNavigation, type DesktopPage } from './navigation';
	import ConnectionStatus from './ConnectionStatus.svelte';
	import Icon from './Icon.svelte';
	import appIcon from '../../../assets/SnenkBotIcon.svg';
	import './theme.css';

	let {
		page,
		title,
		connections,
		requests = [],
		errors = [],
		onNavigate,
		onConfigure,
		onDismissError,
		onBack,
		toolbar,
		children
	}: {
		page: DesktopPage;
		title: string;
		connections: readonly Connection[];
		requests?: readonly ReconfigurationRequest[];
		errors?: readonly ErrorNotice[];
		onNavigate: (page: DesktopPage) => void;
		onConfigure: (connection: Connection) => void;
		onDismissError: (id: string) => Promise<void> | void;
		onBack?: () => void;
		toolbar?: Snippet;
		children: Snippet;
	} = $props();
	let dismissing = $state<string[]>([]);
	let dismissalError = $state('');

	async function dismiss(id: string) {
		if (dismissing.includes(id)) return;
		dismissing = [...dismissing, id];
		dismissalError = '';
		try {
			await onDismissError(id);
		} catch {
			dismissalError = 'This notice could not be dismissed. Try again.';
		} finally {
			dismissing = dismissing.filter((value) => value !== id);
		}
	}
</script>

<div class="desktop-shell">
	<aside class="sidebar">
		<div class="brand">
			<img src={appIcon} alt="" width="28" height="28" /><strong>SnenkBot</strong>
		</div>
		<nav aria-label="Main navigation">
			{#each [false, true] as settings (settings)}
				<div class:settings class="navigation-group">
					{#each desktopNavigation.filter((item) => item.settings === settings) as item (item.id)}
						<button
							class:active={page === item.id}
							aria-current={page === item.id ? 'page' : undefined}
							onclick={() => onNavigate(item.id)}
							><Icon name={item.icon} size={18} /><span>{item.title}</span></button
						>
					{/each}
				</div>
			{/each}
		</nav>
	</aside>
	<div class="workspace">
		<header>
			{#if onBack}<button class="back" aria-label="Back to previous view" onclick={onBack}
					><Icon name="back" size={18} /></button
				>{/if}
			<h1>{title}</h1>
			<div class="toolbar">
				{#if toolbar}{@render toolbar()}{/if}
			</div>
			<ConnectionStatus {connections} {requests} {onConfigure} />
		</header>
		{#if errors.length || dismissalError}
			<div class="notices" aria-label="Application notices">
				{#each errors as notice (notice.id)}
					<div class="notice">
						<span role="status">{notice.message}</span><button
							aria-label="Dismiss notice"
							disabled={dismissing.includes(notice.id)}
							onclick={() => dismiss(notice.id)}><Icon name="close" size={16} /></button
						>
					</div>
				{/each}
				{#if dismissalError}<p role="alert">{dismissalError}</p>{/if}
			</div>
		{/if}
		<main>{@render children()}</main>
	</div>
</div>

<style>
	.desktop-shell {
		display: grid;
		grid-template-columns: 200px minmax(0, 1fr);
		height: 100%;
		min-height: 0;
		background: var(--ink);
	}
	.sidebar {
		background: var(--side);
		border-right: 1px solid var(--border);
		overflow: auto;
	}
	.brand {
		height: 56px;
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 0 16px;
	}
	.brand img {
		flex: none;
	}
	nav {
		padding: 6px 8px;
	}
	.navigation-group {
		display: grid;
		gap: 0;
	}
	.navigation-group.settings {
		margin-top: 22px;
	}
	.navigation-group.settings::before {
		content: '';
		height: 1px;
		background: var(--border);
		margin: 0 12px 18px;
	}
	nav button {
		display: flex;
		align-items: center;
		gap: 10px;
		height: 36px;
		width: 100%;
		padding: 0 10px;
		background: transparent;
		border: 0;
		border-radius: 6px;
		text-align: left;
	}
	nav button:hover {
		background: var(--surface);
	}
	nav button.active {
		background: var(--maroon);
	}
	.workspace {
		display: flex;
		flex-direction: column;
		min-height: 0;
		min-width: 0;
	}
	header {
		height: 56px;
		flex: none;
		display: flex;
		align-items: center;
		gap: 16px;
		padding: 0 24px;
		border-bottom: 1px solid var(--border);
	}
	h1 {
		font-size: 16px;
		margin: 0;
		font-weight: 600;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.toolbar {
		margin-left: auto;
		display: flex;
		align-items: center;
		gap: 8px;
	}
	.back,
	.notice button {
		display: grid;
		place-items: center;
		width: 32px;
		height: 32px;
		flex: none;
		border: 0;
		border-radius: 6px;
		background: var(--surface);
	}
	.notices {
		flex: none;
		max-height: 160px;
		overflow: auto;
		border-bottom: 1px solid var(--border);
		padding: 4px 24px;
	}
	.notice {
		display: flex;
		align-items: center;
		gap: 12px;
		min-height: 40px;
	}
	.notice span {
		flex: 1;
		color: var(--gold);
		overflow-wrap: anywhere;
		font-size: 12px;
		line-height: 1.5;
	}
	.notices p {
		margin: 8px 0;
		color: var(--gold);
		font-size: 12px;
	}
	main {
		display: flex;
		flex-direction: column;
		flex: 1;
		min-height: 0;
		overflow: auto;
	}
	@media (max-width: 1100px) {
		header {
			gap: 12px;
			padding: 0 16px;
		}
	}
</style>
