import { writable } from 'svelte/store';
import { DesktopRequestError, type createDesktopClient } from './desktop';
import type {
	ActionChoices,
	ConfigChoice,
	EditorValueSources,
	ValueSource
} from './contracts/index';

export interface EditorResourceState {
	context: EditorValueSources | null;
	values: readonly ValueSource[];
	valuesLoading: boolean;
	valuesError: string | null;
	choices: readonly ConfigChoice[];
	choiceRequest: ActionChoices | null;
	choicesLoading: boolean;
	choicesError: string | null;
}

/** One selected editor position owns its pickers. Results never outlive that draft revision. */
export function createEditorResourceController(
	client: Pick<ReturnType<typeof createDesktopClient>, 'valueSources' | 'actionChoices'>
) {
	let state: EditorResourceState = {
		context: null,
		values: [],
		valuesLoading: false,
		valuesError: null,
		choices: [],
		choiceRequest: null,
		choicesLoading: false,
		choicesError: null
	};
	const store = writable(state);
	let generation = 0;
	let disposed = false;
	let active = 0;
	let valuesPending: Promise<void> | undefined;
	let choicesPending: Promise<void> | undefined;
	let choiceKey = '';
	let choiceAttempt = 0;
	function publish(change: Partial<EditorResourceState>) {
		if (disposed) return;
		state = { ...state, ...change };
		store.set(state);
	}
	function message(error: unknown) {
		return error instanceof DesktopRequestError
			? error.message
			: 'These values could not be loaded. Try again.';
	}
	function setContext(context: EditorValueSources | null) {
		if (
			disposed ||
			(context?.session_id === state.context?.session_id &&
				context?.expected_revision === state.context?.expected_revision &&
				context?.step_id === state.context?.step_id)
		)
			return;
		generation += 1;
		valuesPending = undefined;
		choicesPending = undefined;
		choiceKey = '';
		choiceAttempt += 1;
		publish({
			context: context ? { ...context } : null,
			values: [],
			choices: [],
			choiceRequest: null,
			valuesLoading: false,
			choicesLoading: false,
			valuesError: null,
			choicesError: null
		});
	}
	function loadValues(): Promise<void> {
		if (disposed || !state.context) return Promise.resolve();
		if (valuesPending) return valuesPending;
		if (active >= 2) {
			publish({ valuesError: 'Value discovery is busy. Try again shortly.' });
			return Promise.resolve();
		}
		const current = generation;
		const context = { ...state.context };
		active += 1;
		publish({ valuesLoading: true, valuesError: null });
		valuesPending = (async () => {
			try {
				await Promise.resolve();
				if (disposed || current !== generation) return;
				const values = await client.valueSources(context);
				if (current === generation) publish({ values });
			} catch (error) {
				if (current === generation) publish({ valuesError: message(error) });
			} finally {
				active -= 1;
				if (current === generation) {
					valuesPending = undefined;
					publish({ valuesLoading: false });
				}
			}
		})();
		return valuesPending;
	}
	function loadChoices(request: ActionChoices): Promise<void> {
		if (disposed || !state.context) return Promise.resolve();
		const key = JSON.stringify(request);
		if (choicesPending && key === choiceKey) return choicesPending;
		if (active >= 2) {
			choiceKey = '';
			choiceAttempt += 1;
			choicesPending = undefined;
			publish({
				choiceRequest: { ...request },
				choices: [],
				choicesLoading: false,
				choicesError: 'Resource discovery is busy. Try again shortly.'
			});
			return Promise.resolve();
		}
		const current = generation;
		const data = { ...request };
		choiceKey = key;
		const requestAttempt = ++choiceAttempt;
		active += 1;
		publish({ choiceRequest: data, choices: [], choicesLoading: true, choicesError: null });
		const pending = (async () => {
			try {
				await Promise.resolve();
				if (disposed || current !== generation || requestAttempt !== choiceAttempt) return;
				const choices = await client.actionChoices(data);
				if (current === generation && requestAttempt === choiceAttempt) publish({ choices });
			} catch (error) {
				if (current === generation && requestAttempt === choiceAttempt)
					publish({ choicesError: message(error) });
			} finally {
				active -= 1;
				if (current === generation && requestAttempt === choiceAttempt) {
					choicesPending = undefined;
					publish({ choicesLoading: false });
				}
			}
		})();
		choicesPending = pending;
		return pending;
	}
	return {
		subscribe: store.subscribe,
		setContext,
		loadValues,
		loadChoices,
		clearError: () => publish({ valuesError: null, choicesError: null }),
		dispose: () => {
			disposed = true;
			generation += 1;
			valuesPending = undefined;
			choicesPending = undefined;
		}
	};
}
