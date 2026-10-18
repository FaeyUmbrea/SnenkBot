import type { CollectionSnapshot } from '../src/lib/ConfigurationCollection.svelte';

/** Describe fixture fields using the same generated contract as native metadata. */
function field(
	id: string,
	label: string,
	kind: import('../src/lib/contracts').ConfigFieldKind,
	choice: string | null = null
) {
	return {
		id,
		label,
		kind,
		description: '',
		introduced_in: 1,
		required: true,
		choice_source: choice ? { key: choice, depends_on: null } : null
	};
}

export const inventory: CollectionSnapshot = {
	actions: [],
	definition: {
		schema: {
			id: 'snenkbot.api.consumer',
			version: 1,
			title: 'Consumer access',
			outputs: [],
			fields: [
				field('name', 'Name', 'text'),
				field('allowed_origins', 'Allowed browser origins', 'string_set'),
				{
					...field('static_folder', 'Static folder', 'directory'),
					required: false,
					description: 'Optional folder to serve. Save changes before copying its hosting URL.'
				},
				field('permissions', 'Permissions', {
					object: [
						field('channels', 'Events', 'string_set', 'api.channels'),
						field('providers', 'Lookups', 'string_set_map', 'api.provider_operations'),
						field('readable_secrets', 'Readable secret names', 'string_set'),
						field('writable_secrets', 'Writable secret names', 'string_set')
					]
				})
			]
		},
		defaults: {
			name: '',
			allowed_origins: [],
			permissions: { channels: [], providers: {}, readable_secrets: [], writable_secrets: [] }
		}
	},
	records: [],
	choices: {
		'api.channels': {
			items: [
				{ value: 'twitch.chat', label: 'Chat messages', detail: 'Receive chat.' },
				{ value: 'twitch.support', label: 'Support events', detail: null }
			]
		},
		'api.provider_operations': {
			groups: [
				{
					id: 'pronouns',
					label: 'Pronouns',
					items: [{ value: 'resolve', label: 'Resolve pronouns', detail: null }]
				}
			]
		}
	}
};
