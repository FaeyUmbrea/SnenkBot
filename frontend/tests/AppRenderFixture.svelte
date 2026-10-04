<script lang="ts">
	import { onMount } from 'svelte';
	import App from '../src/App.svelte';
	import { createApplicationController, type DesktopClient } from '../src/lib/application';
	import { createDesktopClient } from '../src/lib/desktop';
	import type {
		ConfigurationSnapshot,
		DesktopSnapshot,
		EditorSnapshot_Serialize
	} from '../src/lib/contracts/index';
	import { schemas as testSchemas, workflow as testWorkflow } from './fixture';
	import { schemas as showcaseSchemas, workflow as showcaseWorkflow } from './showcase';
	const showcase = new URLSearchParams(location.search).has('showcase');
	const schemas = showcase ? showcaseSchemas : testSchemas;
	const workflow = showcase ? showcaseWorkflow : testWorkflow;
	const title = showcase ? 'Welcome message' : 'Stream details';
	const snapshot: DesktopSnapshot = {
		sequence: 0,
		startup: { state: 'ready' },
		workflows: [
			{
				id: workflow.id,
				title,
				enabled: true,
				revision: 1,
				step_count: workflow.steps.length,
				has_steps: true,
				error: null,
				category: 'Broadcast',
				trigger_summary: 'Manual',
				capabilities: [],
				capability_titles: {},
				integration_usage: {}
			}
		],
		connections: [],
		reconfiguration_requests: [],
		errors: new URLSearchParams(location.search).has('notice')
			? [
					{
						id: 'fixture-notice',
						message: 'A configured connection needs attention. Open its settings to reconnect.'
					}
				]
			: [],
		last_completed: null,
		pending_input: null,
		twitch_authentication: null
	};
	let editor: EditorSnapshot_Serialize = {
		session_id: 'editor',
		revision: 0,
		draft: {
			name: title,
			enabled: true,
			workflow,
			triggers: []
		},
		dirty: false,
		can_undo: false,
		can_redo: false,
		saved_revision: 1
	};
	const configuration: ConfigurationSnapshot = {
		session_id: 'render-fixture',
		revision: 0,
		module: 'obs',
		schema: {
			id: 'snenkbot.obs.connection',
			version: 1,
			title: 'OBS Studio',
			outputs: [],
			fields: [
				{
					id: 'enabled',
					label: 'Enabled',
					description: '',
					introduced_in: 1,
					kind: 'toggle',
					required: true,
					choice_source: null
				},
				{
					id: 'host',
					label: 'Host',
					description: '',
					introduced_in: 1,
					kind: 'text',
					required: true,
					choice_source: null
				},
				{
					id: 'port',
					label: 'Port',
					description: '',
					introduced_in: 1,
					kind: 'integer',
					required: true,
					choice_source: null
				},
				{
					id: 'tls',
					label: 'Secure connection',
					description: '',
					introduced_in: 1,
					kind: 'toggle',
					required: true,
					choice_source: null
				}
			]
		},
		defaults: { enabled: false, host: '127.0.0.1', port: 4455, tls: false },
		values: {
			enabled: true,
			host: showcase ? '127.0.0.1' : '192.168.178.44',
			port: 4455,
			tls: false
		}
	};
	const client: DesktopClient = {
		...createDesktopClient(async () => {
			throw new Error('Unexpected native request');
		}),
		actionDefinitions: async () =>
			schemas.map((schema) => ({ schema, defaults: { title: 'Native default' } })),
		twitchAccounts: async () => ({
			broadcaster: { user_id: '123', login: showcase ? 'example_channel' : 'faey' },
			bot: { user_id: '456', login: 'snenkbot' }
		}),
		twitchLoginSnapshot: async () => null,
		historyPage: async () => ({ entries: [], next_cursor: null }),
		openEditor: async () => editor,
		openConfiguration: async () => configuration,
		obsPasswordStatus: async () => 'empty' as const,
		closeEditor: async () => undefined,
		closeConfiguration: async () => undefined,
		applyEditorEdit: async (request) => {
			const edit = request.edit;
			if (edit.operation === 'rename')
				editor = { ...editor, draft: { ...editor.draft, name: edit.name } };
			editor = { ...editor, revision: editor.revision + 1, dirty: true, can_undo: true };
			return editor;
		},
		saveEditor: async () => ({
			snapshot: { ...editor, dirty: false, saved_revision: 2 },
			notices: []
		})
	};
	const controller = createApplicationController(client, (callbacks) => {
		callbacks.onSnapshot(snapshot);
		return {
			ready: Promise.resolve(),
			dispose: () => undefined,
			refresh: async () => undefined
		};
	});
	onMount(() => {
		if (showcase) void controller.openWorkflow(workflow.id);
	});
</script>

<App {client} {controller} />

<style>
	:global(body) {
		margin: 0;
		height: 100vh;
		background: #0b0808;
	}
	:global(#fixture) {
		height: 100%;
	}
</style>
