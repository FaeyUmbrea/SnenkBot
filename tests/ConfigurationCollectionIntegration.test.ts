import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import ConfigurationCollection from '../src/lib/ConfigurationCollection.svelte';
import { createConfigurationCollection } from '../src/lib/configuration-collection';
import CredentialField from '../src/lib/CredentialField.svelte';
import { createDesktopClient } from '../src/lib/desktop';
import type { ConfigurationSnapshot } from '../src/lib/contracts';

import { inventory } from './consumer-fixture';

const owner: ConfigurationSnapshot = {
	session_id: 'api-session',
	revision: 0,
	module: 'api',
	schema: inventory.definition.schema,
	defaults: {},
	values: {},
	credential: null,
	authorization: null,
	actions: [],
	authorization_ready: false,
	collection: {
		id: 'api.consumers',
		title: 'Consumers',
		item_label: 'consumer',
		description: 'Manage applications that can access SnenkBot.'
	}
};
afterEach(cleanup);

describe('API consumer administration', () => {
	it('grants only explicit selections and reveals only the freshly created token', async () => {
		const transport = vi.fn(async (command: string) =>
			command === 'configuration_collection_load' ? inventory : 'fixture-token'
		);
		const view = render(ConfigurationCollection, {
			title: 'Consumers',
			...createConfigurationCollection(createDesktopClient(transport), owner)
		});
		await waitFor(() => expect(view.getByRole('button', { name: 'Add entry' })).toBeTruthy());
		await fireEvent.click(view.getByRole('button', { name: 'Add entry' }));
		await waitFor(() => expect(view.getByLabelText('Name')).toBeTruthy());
		await fireEvent.input(view.getByLabelText('Name'), { target: { value: 'Chat' } });
		await fireEvent.input(view.getByLabelText('Add Allowed browser origins'), {
			target: { value: 'http://localhost:47831' }
		});
		await fireEvent.keyDown(view.getByLabelText('Add Allowed browser origins'), { key: 'Enter' });
		await fireEvent.click(view.getByRole('checkbox', { name: /Chat messages/ }));
		await fireEvent.click(view.getByLabelText('Resolve pronouns'));
		await fireEvent.click(view.getByRole('button', { name: 'Create entry' }));
		await waitFor(() =>
			expect(view.getByLabelText('New credential')).toHaveProperty('value', 'fixture-token')
		);
		expect(transport).toHaveBeenCalledWith('configuration_collection_create', {
			request: {
				session_id: 'api-session',
				collection_id: 'api.consumers',
				values: {
					name: 'Chat',
					allowed_origins: ['http://localhost:47831'],
					permissions: {
						channels: ['twitch.chat'],
						providers: { pronouns: ['resolve'] },
						readable_secrets: [],
						writable_secrets: []
					}
				}
			}
		});
		await fireEvent.click(view.getByRole('button', { name: 'Hide credential' }));
		expect(view.queryByLabelText('New credential')).toBeNull();
	});

	it('retains an unsuccessful draft and does not expose raw backend diagnostics', async () => {
		const transport = vi.fn(async (command: string) => {
			if (command === 'configuration_collection_load') return inventory;
			throw new Error('/private/credential/backend');
		});
		const view = render(ConfigurationCollection, {
			title: 'Consumers',
			...createConfigurationCollection(createDesktopClient(transport), owner)
		});
		await waitFor(() => expect(view.getByRole('button', { name: 'Add entry' })).toBeTruthy());
		await fireEvent.click(view.getByRole('button', { name: 'Add entry' }));
		await waitFor(() => expect(view.getByLabelText('Name')).toBeTruthy());
		await fireEvent.input(view.getByLabelText('Name'), { target: { value: 'Keep me' } });
		await fireEvent.click(view.getByRole('button', { name: 'Create entry' }));
		await waitFor(() => expect(view.getByRole('alert')).toBeTruthy());
		expect(view.getByLabelText('Name')).toHaveProperty('value', 'Keep me');
		expect(view.container.textContent).not.toContain('/private/');
	});

	it('uses the owner collection identity for record actions and directory browsing', async () => {
		const transport = vi.fn(async (command: string) =>
			command === 'configuration_collection_action' ? 'replacement' : '/selected/output'
		);
		const collection = createConfigurationCollection(createDesktopClient(transport), owner);
		expect(await collection.act('existing', 'rotate')).toBe('replacement');
		expect(await collection.pickDirectory(['nested', 'folder'], '/old/output')).toBe(
			'/selected/output'
		);
		expect(transport).toHaveBeenCalledWith('configuration_collection_action', {
			request: {
				session_id: owner.session_id,
				collection_id: owner.collection!.id,
				record_id: 'existing',
				action_id: 'rotate'
			}
		});
		expect(transport).toHaveBeenCalledWith('configuration_pick_directory', {
			request: {
				session_id: owner.session_id,
				collection_id: owner.collection!.id,
				field_path: ['nested', 'folder'],
				current_path: '/old/output'
			}
		});
	});

	it('keeps collection owners and record edit sessions separate', async () => {
		const record = {
			...owner,
			session_id: 'consumer-edit',
			revision: 3,
			values: { name: 'Existing' }
		};
		const transport = vi.fn(async (command: string) => {
			if (command === 'configuration_collection_load') return inventory;
			if (command === 'configuration_open') return record;
			if (command === 'configuration_save')
				return { snapshot: { ...record, revision: 4 }, notices: [] };
			return undefined;
		});
		const client = createDesktopClient(transport);
		const first = createConfigurationCollection(client, owner);
		const second = createConfigurationCollection(client, {
			...owner,
			session_id: 'other-api-session'
		});
		await first.load();
		await second.load();
		expect(transport).toHaveBeenCalledWith('configuration_collection_load', {
			request: { session_id: 'api-session', collection_id: 'api.consumers' }
		});
		expect(transport).toHaveBeenCalledWith('configuration_collection_load', {
			request: { session_id: 'other-api-session', collection_id: 'api.consumers' }
		});
		await first.open('existing');
		await first.save('existing', { name: 'Updated' });
		expect(transport).toHaveBeenCalledWith('configuration_save', {
			request: {
				session_id: 'consumer-edit',
				expected_revision: 3,
				values: { name: 'Updated' }
			}
		});
		await second.remove('another');
		expect(transport).toHaveBeenCalledWith('configuration_collection_remove', {
			request: {
				session_id: 'other-api-session',
				collection_id: 'api.consumers',
				record_id: 'another'
			}
		});
		await first.close();
		expect(transport).toHaveBeenCalledWith('configuration_close', {
			request: { session_id: 'consumer-edit' }
		});
	});

	it('does not let an old credential presence read replace a successful save', async () => {
		let finish!: (value: string) => void;
		const pending = new Promise<string>((resolve) => {
			finish = resolve;
		});
		const transport = vi.fn(async (command: string) =>
			command === 'configuration_credential_status' ? pending : 'stored'
		);
		const client = createDesktopClient(transport);
		const view = render(CredentialField, {
			descriptor: {
				id: 'provider-key',
				label: 'Provider API key',
				emptyDescription: 'No key stored.',
				storedDescription: 'A key is stored. Enter a new key to replace it.',
				unavailableDescription: 'Credential status is unavailable.',
				saveLabel: 'Save key',
				savedMessage: 'Key saved.',
				saveError: 'The key could not be stored.',
				removeLabel: 'Remove key',
				removedMessage: 'Key removed.'
			},
			contextKey: 'provider-session',
			onRefresh: () =>
				client.configurationCredentialStatus({
					session_id: 'provider-session',
					credential_id: 'provider-key'
				}),
			onSave: (value) =>
				client.saveConfigurationCredential({
					session_id: 'provider-session',
					credential_id: 'provider-key',
					value
				})
		});
		await fireEvent.change(view.getByLabelText('Provider API key'), {
			target: { value: 'fixture-only' }
		});
		await fireEvent.click(view.getByRole('button', { name: 'Save key' }));
		await waitFor(() => expect(view.getByText('Key saved.')).toBeTruthy());
		finish('empty');
		await Promise.resolve();
		expect(view.getByText('A key is stored. Enter a new key to replace it.')).toBeTruthy();
		expect(view.getByLabelText('Provider API key')).toHaveProperty('value', '');
	});
});
