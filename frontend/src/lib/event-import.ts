import { DesktopRequestError, type createDesktopClient } from './desktop';
/** Forward the supported import envelope to the native domain validator. */
export async function importSupportHistory(
	client: ReturnType<typeof createDesktopClient>,
	value: unknown
): Promise<string> {
	if (
		!value ||
		typeof value !== 'object' ||
		!('version' in value) ||
		value.version !== 1 ||
		!('events' in value) ||
		!Array.isArray(value.events)
	) {
		throw new DesktopRequestError({
			code: 'invalid_values',
			message: 'Choose an overlay events.json file with version 1.'
		});
	}
	const count = await client.importSupportHistory({ version: 1, events: value.events });
	return `${count} support events imported. The original file is unchanged.`;
}
