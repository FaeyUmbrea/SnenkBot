<script lang="ts">
	import type { ConnectionStatus, ReconfigurationRequest } from './contracts/index';
	import Icon from './Icon.svelte';

	let {
		connections,
		requests = [],
		onConfigure
	}: {
		connections: readonly ConnectionStatus[];
		requests?: readonly ReconfigurationRequest[];
		onConfigure: (connection: ConnectionStatus) => void;
	} = $props();
	let open = $state(false);
	let root: HTMLDetailsElement;
	let summary: HTMLElement;
	const connected = $derived(connections.filter((item) => item.state === 'connected').length);
	const severity = $derived(
		connections.some((item) => item.state === 'error')
			? 'error'
			: connections.some((item) => item.state === 'connecting')
				? 'connecting'
				: connections.length > 0 && connected === connections.length
					? 'connected'
					: 'inactive'
	);
	const stateLabels = {
		connected: 'Connected',
		connecting: 'Connecting',
		error: 'Connection error',
		inactive: 'Inactive'
	};

	function close(restore = false) {
		open = false;
		if (restore) summary?.focus();
	}
</script>

<svelte:window
	onpointerdown={(event) => {
		if (open && event.target instanceof Node && !root.contains(event.target)) close();
	}}
	onkeydown={(event) => {
		if (open && event.key === 'Escape') {
			event.preventDefault();
			close(true);
		}
	}}
/>

<details class="connection-status" bind:this={root} bind:open>
	<summary
		bind:this={summary}
		onclick={(event) => {
			event.preventDefault();
			open = !root.open;
		}}
		aria-label={`Connections: ${connected} of ${connections.length} connected`}
	>
		<span class="pip {severity}"></span>
		<span>Connected {connected}/{connections.length}</span>
	</summary>
	<div class="connection-popover" role="region" aria-label="Connection overview">
		<div class="heading">
			<strong>Connections</strong><button
				aria-label="Close connection overview"
				onclick={() => close(true)}><Icon name="close" size={16} /></button
			>
		</div>
		{#if connections.length === 0}
			<p>No connections configured yet.</p>
		{:else}
			<ul>
				{#each connections as item (`${item.integration}:${item.connection}`)}
					<li>
						<div class="connection-title">
							<span class="pip {item.state}"></span><strong>{item.title}</strong>
						</div>
						<p class="state-label">{stateLabels[item.state]}</p>
						{#if item.detail}<p>{item.detail}</p>{/if}
						{#each requests.filter((request) => request.integration === item.integration && request.connection === item.connection) as request (request.reason)}
							<p class="reconnect">{request.reason}</p>
						{/each}
						<button
							class="configure"
							onclick={() => {
								close();
								onConfigure(item);
							}}>Configure <Icon name="settings" size={14} /></button
						>
					</li>
				{/each}
			</ul>
		{/if}
	</div>
</details>

<style>
	.connection-status {
		position: relative;
		flex: none;
	}
	summary {
		list-style: none;
		display: flex;
		align-items: center;
		gap: 10px;
		min-height: 32px;
		padding: 0 12px;
		border-radius: 7px;
		background: var(--surface);
		cursor: pointer;
	}
	summary::-webkit-details-marker {
		display: none;
	}
	.pip {
		width: 7px;
		height: 7px;
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
	.connection-popover {
		position: absolute;
		right: 0;
		top: calc(100% + 8px);
		z-index: 20;
		width: min(360px, calc(100vw - 32px));
		max-height: min(560px, calc(100vh - 84px));
		overflow: auto;
		border: 1px solid var(--border);
		border-radius: 10px;
		background: var(--side);
		box-shadow: 0 12px 32px #0006;
		padding: 16px;
	}
	.heading {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 16px;
	}
	button {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 8px;
		min-height: 32px;
		background: var(--surface);
		border: 0;
		border-radius: 6px;
		padding: 6px 8px;
	}
	ul {
		margin: 12px 0 0;
		padding: 0;
		list-style: none;
	}
	li {
		padding: 12px 0;
		border-top: 1px solid var(--border);
	}
	li:last-child {
		padding-bottom: 0;
	}
	.connection-title {
		display: flex;
		align-items: center;
		gap: 10px;
	}
	p {
		margin: 8px 0;
		color: var(--muted);
		font-size: 12px;
		line-height: 1.5;
		overflow-wrap: anywhere;
	}
	.state-label {
		margin-left: 17px;
	}
	.reconnect {
		color: var(--gold);
	}
</style>
