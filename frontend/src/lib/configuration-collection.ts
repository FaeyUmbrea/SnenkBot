import type { ConfigurationSnapshot } from './contracts';
import { DesktopRequestError, type createDesktopClient } from './desktop';

/** Connect the shared record editor to the collection declared by its settings session. */
export function createConfigurationCollection(
	client: ReturnType<typeof createDesktopClient>,
	owner: ConfigurationSnapshot
) {
	const collection = owner.collection;
	if (!collection) throw new Error('This settings form has no collection.');
	const identity = { session_id: owner.session_id, collection_id: collection.id };
	let session: ConfigurationSnapshot | null = null;

	/** Release the record's optimistic-save baseline when the editor closes. */
	async function close() {
		if (session) await client.closeConfiguration({ session_id: session.session_id });
		session = null;
	}

	/** Load an independent native baseline for the requested record. */
	async function open(id: string) {
		const next = await client.openConfiguration({ module: owner.module, record_id: id });
		try {
			await close();
		} catch (cause) {
			await client.closeConfiguration({ session_id: next.session_id });
			throw cause;
		}
		session = next;
		return session.values;
	}

	/** Preserve the generated value shape and native conflict checks on every save. */
	async function save(id: string | null, values: Record<string, unknown>) {
		if (!id) return (await client.collectionCreate({ ...identity, values })) ?? undefined;
		if (!session)
			throw new DesktopRequestError({
				code: 'missing_session',
				message: 'Reopen this entry before saving.'
			});
		session = (
			await client.saveConfiguration({
				session_id: session.session_id,
				expected_revision: session.revision,
				values
			})
		).snapshot;
	}

	return {
		load: () => client.collectionLoad(identity),
		open,
		close,
		save,
		act: async (record_id: string, action_id: string) =>
			(await client.collectionAction({ ...identity, record_id, action_id })) ?? undefined,
		pickDirectory: (field_path: string[], current_path: string | null) =>
			client.pickConfigurationDirectory({ ...identity, field_path, current_path }),
		remove: (record_id: string) => client.collectionRemove({ ...identity, record_id })
	};
}
