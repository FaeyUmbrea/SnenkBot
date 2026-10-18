import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { afterEach, expect, it, vi } from 'vitest';
import ConfigurationDirectory from '../src/lib/ConfigurationDirectory.svelte';
import type { ConfigurationCatalogEntry } from '../src/lib/contracts';

afterEach(cleanup);

/** Create configured and unconfigured capabilities to exercise permanent settings access. */
function entries(): ConfigurationCatalogEntry[] {
	return Array.from({ length: 12 }, (_, index) => ({
		id: `record-${index}`,
		title: `Studio ${index}`,
		category: 'broadcast',
		module: 'obs',
		record_id: `connection-${index}`,
		error: null,
		configured: index < 10
	}));
}

it('keeps every singleton accessible regardless of saved state and supports search', async () => {
	const onSelect = vi.fn();
	const records = entries();
	const view = render(ConfigurationDirectory, {
		title: 'Broadcast Apps',
		entries: records,
		onSelect
	});
	expect(view.getByRole('button', { name: /Studio 10/ })).toBeTruthy();
	expect(view.queryByRole('button', { name: /^Add$/ })).toBeNull();
	await fireEvent.input(view.getByRole('searchbox'), { target: { value: 'Studio 9' } });
	expect(view.queryByRole('button', { name: /Studio 0/ })).toBeNull();
	await fireEvent.click(view.getByRole('button', { name: /Studio 9/ }));
	expect(onSelect).toHaveBeenCalledWith(records[9]);
	await fireEvent.input(view.getByRole('searchbox'), { target: { value: '' } });
	await fireEvent.click(view.getByRole('button', { name: /Studio 10/ }));
	expect(onSelect).toHaveBeenLastCalledWith(records[10]);
});
