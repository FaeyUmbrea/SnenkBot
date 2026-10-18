import { describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import { createEditorResourceController } from '../src/lib/editor-resources';
import { DesktopRequestError } from '../src/lib/desktop';
import type { ConfigChoice, ValueSource } from '../src/lib/contracts/index';

function deferred<T>() {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((done) => {
		resolve = done;
	});
	return { promise, resolve };
}
const context = { session_id: 'session', expected_revision: 4, step_id: 'step' };
const choicesRequest = { action_id: 'obs.action', field_id: 'item', depends_on: 'scene-one' };
const choice: ConfigChoice = { value: 'stable', label: 'Friendly', detail: null };
const source: ValueSource = {
	source: 'trigger',
	source_id: 'trigger',
	output_id: 'title',
	label: 'Title',
	detail: 'Stream title',
	optional: true,
	fallback_kind: 'text'
};
function harness() {
	const client = {
		valueSources: vi.fn(async () => [source]),
		actionChoices: vi.fn(async () => [choice])
	};
	return { client, resources: createEditorResourceController(client) };
}

describe('editor resource ownership', () => {
	it('uses request identity when a dependency changes away and back', async () => {
		const h = harness();
		const old = deferred<ConfigChoice[]>();
		h.client.actionChoices.mockReturnValueOnce(old.promise);
		h.resources.setContext(context);
		const first = h.resources.loadChoices(choicesRequest);
		await Promise.resolve();
		await h.resources.loadChoices({ ...choicesRequest, depends_on: 'scene-two' });
		await h.resources.loadChoices(choicesRequest);
		old.resolve([{ ...choice, label: 'Stale same-dependency response' }]);
		await first;
		expect(get(h.resources).choices).toEqual([choice]);
		expect(h.client.actionChoices).toHaveBeenCalledTimes(3);
	});

	it('discovers only on request and coalesces identical pending requests', async () => {
		const h = harness();
		h.resources.setContext(context);
		expect(h.client.valueSources).not.toHaveBeenCalled();
		const first = h.resources.loadValues();
		expect(h.resources.loadValues()).toBe(first);
		await first;
		expect(h.client.valueSources).toHaveBeenCalledExactlyOnceWith(context);
		expect(get(h.resources).values).toEqual([source]);
		const options = h.resources.loadChoices(choicesRequest);
		expect(h.resources.loadChoices(choicesRequest)).toBe(options);
		await options;
		expect(h.client.actionChoices).toHaveBeenCalledExactlyOnceWith(choicesRequest);
		expect(get(h.resources).choices).toEqual([choice]);
	});
	it('invalidates results when the draft revision, selection or session changes', async () => {
		for (const next of [
			{ ...context, expected_revision: 5 },
			{ ...context, step_id: 'other' },
			{ ...context, session_id: 'other' },
			null
		]) {
			const h = harness();
			const pending = deferred<ValueSource[]>();
			h.client.valueSources.mockReturnValue(pending.promise);
			h.resources.setContext(context);
			const load = h.resources.loadValues();
			await Promise.resolve();
			h.resources.setContext(next);
			pending.resolve([source]);
			await load;
			expect(get(h.resources).values).toEqual([]);
			expect(get(h.resources).valuesLoading).toBe(false);
		}
	});
	it('keeps current dependency choices when an older response arrives later', async () => {
		const h = harness();
		const old = deferred<ConfigChoice[]>();
		h.client.actionChoices.mockReturnValueOnce(old.promise);
		h.resources.setContext(context);
		const first = h.resources.loadChoices(choicesRequest);
		await Promise.resolve();
		await h.resources.loadChoices({ ...choicesRequest, depends_on: 'scene-two' });
		old.resolve([{ ...choice, label: 'Old resource' }]);
		await first;
		expect(get(h.resources).choices).toEqual([choice]);
		expect(get(h.resources).choiceRequest?.depends_on).toBe('scene-two');
	});
	it('bounds outstanding discovery across context changes and permits explicit retry', async () => {
		const h = harness();
		const old = deferred<ValueSource[]>();
		h.client.valueSources.mockReturnValue(old.promise);
		h.resources.setContext(context);
		const first = h.resources.loadValues();
		await Promise.resolve();
		h.resources.setContext({ ...context, expected_revision: 5 });
		const second = h.resources.loadValues();
		await Promise.resolve();
		h.resources.setContext({ ...context, expected_revision: 6 });
		await h.resources.loadChoices(choicesRequest);
		expect(h.client.actionChoices).not.toHaveBeenCalled();
		expect(get(h.resources).choicesError).toContain('busy');
		old.resolve([]);
		await Promise.all([first, second]);
		await h.resources.loadChoices(choicesRequest);
		expect(get(h.resources).choices).toEqual([choice]);
		expect(get(h.resources).choicesError).toBeNull();
	});
	it('retains cached values on failure and hides unstructured diagnostics', async () => {
		const h = harness();
		h.resources.setContext(context);
		await h.resources.loadValues();
		h.client.valueSources.mockRejectedValueOnce(new Error('/private/backend'));
		await h.resources.loadValues();
		expect(get(h.resources).values).toEqual([source]);
		expect(get(h.resources).valuesError).not.toContain('/private/');
		h.resources.clearError();
		expect(get(h.resources).valuesError).toBeNull();
		h.client.actionChoices.mockRejectedValueOnce(
			new DesktopRequestError({ code: 'unavailable', message: 'Connect OBS to load resources.' })
		);
		await h.resources.loadChoices(choicesRequest);
		expect(get(h.resources).choicesError).toBe('Connect OBS to load resources.');
	});
	it('does not publish or invoke new work after disposal', async () => {
		const h = harness();
		const pending = deferred<ConfigChoice[]>();
		h.client.actionChoices.mockReturnValue(pending.promise);
		h.resources.setContext(context);
		const load = h.resources.loadChoices(choicesRequest);
		await Promise.resolve();
		h.resources.dispose();
		const before = get(h.resources);
		pending.resolve([choice]);
		await load;
		await h.resources.loadValues();
		await h.resources.loadChoices(choicesRequest);
		expect(get(h.resources)).toBe(before);
		expect(h.client.valueSources).not.toHaveBeenCalled();
		expect(h.client.actionChoices).toHaveBeenCalledTimes(1);
	});
});
