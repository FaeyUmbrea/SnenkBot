import { fireEvent, render, waitFor } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import WorkflowCanvas from '../src/lib/WorkflowCanvas.svelte';
import ActionCatalog from '../src/lib/ActionCatalog.svelte';
import { WORKFLOW_DRAG_TYPE } from '../src/lib/drag';
import type { CanvasProps } from '../src/lib/canvas';
import type { WorkflowDrag } from '../src/lib/drag';
import type { StepKind } from '../src/lib/contracts/index';
import { catalog, schemas, step, workflow, workflowTitles } from './fixture';

function props(overrides: Partial<CanvasProps> = {}): CanvasProps {
	return {
		workflow: structuredClone(workflow),
		schemas,
		catalog,
		workflowTitles,
		selectedStepId: null,
		summaryFields: { 'obs.set_game': ['game', 'enabled'] },
		onSelect: vi.fn(),
		onMove: vi.fn(),
		onAdd: vi.fn(),
		onInputChange: vi.fn(),
		onEditStep: vi.fn(),
		onSourcePicker: vi.fn(),
		...overrides
	};
}

function transfer(payload: WorkflowDrag) {
	const contents = new Map([[WORKFLOW_DRAG_TYPE, JSON.stringify(payload)]]);
	return {
		types: [WORKFLOW_DRAG_TYPE],
		getData: (type: string) => contents.get(type) ?? '',
		setData: (type: string, value: string) => contents.set(type, value),
		effectAllowed: 'all',
		dropEffect: 'none'
	};
}

describe('WorkflowCanvas', () => {
	it('renders all control rows and meaningful values without raw identifiers', () => {
		const view = render(WorkflowCanvas, props());
		expect(view.getAllByText('Otherwise')).toHaveLength(2);
		expect(view.getAllByText('End If')).toHaveLength(2);
		expect(view.getByLabelText('Delay seconds')).toHaveProperty('value', '1.25');
		expect(view.getByLabelText('Title')).toHaveProperty('value', 'Sunday stories with Faey');
		expect(view.getByLabelText('Game')).toHaveProperty('value', 'Guild Wars 2');
		expect(view.getByLabelText('Update category')).toHaveProperty('checked', true);
		expect(view.getByText('Run Announce the stream')).toBeTruthy();
		expect(view.getByText('Set game · Game name')).toBeTruthy();
		expect(view.container.textContent).not.toContain('obs.set_title');
		expect(view.container.querySelectorAll('.step.control > .sequence')).toHaveLength(0);
	});

	it('selects a step, deselects via canvas background, and preserves control clicks', async () => {
		const callbacks = props();
		const view = render(WorkflowCanvas, callbacks);
		await fireEvent.click(view.getByRole('button', { name: 'Set stream title' }));
		expect(callbacks.onSelect).toHaveBeenLastCalledWith('title');
		await fireEvent.click(view.getByLabelText('Title'));
		expect(callbacks.onSelect).toHaveBeenCalledTimes(1);
		await fireEvent.pointerDown(view.getByRole('region', { name: 'Workflow canvas' }));
		expect(callbacks.onSelect).toHaveBeenLastCalledWith(null);
	});

	it('sends literal and toggle edits without mutating the supplied workflow', async () => {
		const callbacks = props();
		const view = render(WorkflowCanvas, callbacks);
		await fireEvent.change(view.getByLabelText('Title'), { target: { value: 'Hi' } });
		expect(callbacks.onInputChange).toHaveBeenLastCalledWith('title', 'title', { Literal: 'Hi' });
		expect(callbacks.workflow.steps[0].kind).toMatchObject({
			Action: { inputs: { title: { Literal: 'Sunday stories with Faey' } } }
		});
		await waitFor(() =>
			expect(view.getByLabelText('Update category')).toHaveProperty('disabled', false)
		);
		await fireEvent.click(view.getByLabelText('Update category'));
		expect(callbacks.onInputChange).toHaveBeenLastCalledWith('game', 'enabled', { Literal: false });
	});

	it('retains the entered draft and reports a rejected edit', async () => {
		const callbacks = props({
			onInputChange: vi.fn().mockRejectedValue(new Error('This title is unavailable.'))
		});
		const view = render(WorkflowCanvas, callbacks);
		await fireEvent.change(view.getByLabelText('Title'), { target: { value: 'Try again' } });
		await waitFor(() =>
			expect(view.getByRole('alert').textContent).toContain('This title is unavailable.')
		);
		expect(view.getByLabelText('Title')).toHaveProperty('value', 'Try again');
		expect(view.getByLabelText('Title')).toHaveProperty('disabled', false);
	});

	it('opens the source picker with the full source and field identity', async () => {
		const callbacks = props();
		const view = render(WorkflowCanvas, callbacks);
		await fireEvent.click(view.getByRole('button', { name: 'Choose source for Title' }));
		expect(callbacks.onSourcePicker).toHaveBeenCalledWith('title', 'title', {
			Literal: 'Sunday stories with Faey'
		});
	});

	it('edits the exact delay duration and supports tiny values', async () => {
		const callbacks = props();
		const view = render(WorkflowCanvas, callbacks);
		await fireEvent.change(view.getByLabelText('Delay seconds'), { target: { value: '0.003' } });
		expect(callbacks.onEditStep).toHaveBeenCalledWith('delay', { Delay: { millis: 3 } });
	});

	it('sends exactly one precise cross-branch operation per successful drop', async () => {
		const callbacks = props();
		const view = render(WorkflowCanvas, callbacks);
		const dataTransfer = transfer({ type: 'step', workflowId: workflow.id, stepId: 'title' });
		const slot = view.container.querySelector('[data-insertion="if:Else:else-message"]');
		expect(slot).toBeTruthy();
		await fireEvent.drop(slot!, { dataTransfer });
		expect(callbacks.onMove).toHaveBeenCalledExactlyOnceWith(
			'title',
			{ Branch: { parent_id: 'if', branch: 'Else' } },
			{ Before: { step_id: 'else-message' } }
		);
		expect(callbacks.workflow.steps[0].id).toBe('title');
		await waitFor(() => expect(callbacks.onSelect).toHaveBeenCalledWith('title'));
		expect(document.activeElement?.getAttribute('data-step-title')).toBe('title');
	});

	it('rejects cycles, invalid anchors, foreign workflows and malformed payloads', async () => {
		const callbacks = props();
		const view = render(WorkflowCanvas, callbacks);
		const nestedSlot = view.container.querySelector('[data-insertion="nested-if:Then:delay"]');
		await fireEvent.drop(nestedSlot!, {
			dataTransfer: transfer({ type: 'step', workflowId: workflow.id, stepId: 'if' })
		});
		await fireEvent.drop(nestedSlot!, {
			dataTransfer: transfer({ type: 'step', workflowId: 'other', stepId: 'title' })
		});
		await fireEvent.drop(nestedSlot!, { dataTransfer: { getData: () => '{broken' } });
		expect(callbacks.onMove).not.toHaveBeenCalled();
	});

	it('marks valid insertion positions during a drag and clears them afterwards', async () => {
		const callbacks = props();
		const view = render(WorkflowCanvas, callbacks);
		const handle = view.getByRole('button', { name: 'Move Set stream title' });
		const dataTransfer = transfer({ type: 'step', workflowId: workflow.id, stepId: 'title' });
		await fireEvent.dragStart(handle, { dataTransfer });
		const slot = view.container.querySelector('[data-insertion="if:Then:then-message"]');
		await fireEvent.dragOver(slot!, { dataTransfer });
		expect(slot?.classList.contains('active')).toBe(true);
		await fireEvent.dragEnd(handle);
		expect(slot?.classList.contains('active')).toBe(false);
	});

	it('adds catalog actions by stable ID at the exact drop position', async () => {
		const callbacks = props();
		const view = render(WorkflowCanvas, callbacks);
		const slot = view.container.querySelector('[data-insertion="if:Then:then-message"]');
		await fireEvent.drop(slot!, {
			dataTransfer: transfer({ type: 'catalog', catalogId: 'delay-action' })
		});
		expect(callbacks.onAdd).toHaveBeenCalledExactlyOnceWith(
			{ Delay: { millis: 1000 } },
			{ Branch: { parent_id: 'if', branch: 'Then' } },
			{ Before: { step_id: 'then-message' } }
		);
		expect(callbacks.onMove).not.toHaveBeenCalled();
	});

	it('provides keyboard movement within siblings and a cross-branch position picker', async () => {
		const callbacks = props();
		const view = render(WorkflowCanvas, callbacks);
		await fireEvent.keyDown(view.getByRole('button', { name: 'Set game' }), {
			key: 'ArrowUp',
			altKey: true
		});
		expect(callbacks.onMove).toHaveBeenCalledWith('game', 'Root', { Before: { step_id: 'title' } });
		await waitFor(() =>
			expect(view.getByRole('button', { name: 'Move Set game' })).toHaveProperty('disabled', false)
		);
		await fireEvent.click(view.getByRole('button', { name: 'Move Set game' }));
		await fireEvent.change(view.getByLabelText('Move destination'), { target: { value: '4' } });
		await fireEvent.change(view.getByLabelText('Move position'), {
			target: { value: 'else-message' }
		});
		await fireEvent.click(view.getByRole('button', { name: /^Move$/ }));
		expect(callbacks.onMove).toHaveBeenLastCalledWith(
			'game',
			{ Branch: { parent_id: 'if', branch: 'Else' } },
			{ Before: { step_id: 'else-message' } }
		);
	});

	it('shows unavailable actions with saved values and explains a missing catalog', async () => {
		const unknown = props({ schemas: [], catalog: [] });
		const view = render(WorkflowCanvas, unknown);
		expect(view.getAllByText('Action unavailable').length).toBeGreaterThan(0);
		expect(view.getByLabelText('Title')).toHaveProperty('value', 'Sunday stories with Faey');
		await fireEvent.click(view.getAllByRole('button', { name: 'Add action at end' })[0]);
		expect(
			view.getByText('The action library is unavailable. Reload the workflow to try again.')
		).toBeTruthy();
	});

	it('makes sequential one-or-more semantics explicit', () => {
		const callbacks = props({
			workflow: { ...workflow, steps: [step('one', { OneOrMore: { steps: [] } })] }
		});
		const view = render(WorkflowCanvas, callbacks);
		expect(view.getByText('One or more')).toHaveProperty(
			'title',
			'All children run; at least one must succeed.'
		);
	});

	it('prevents repeated drop operations while the backend is saving', async () => {
		let finish: () => void = () => {};
		const saving = new Promise<void>((resolve) => {
			finish = resolve;
		});
		const callbacks = props({ onMove: vi.fn(() => saving) });
		const view = render(WorkflowCanvas, callbacks);
		const slot = view.container.querySelector('[data-insertion="if:Then:then-message"]');
		const dataTransfer = transfer({ type: 'step', workflowId: workflow.id, stepId: 'title' });
		await fireEvent.drop(slot!, { dataTransfer });
		await fireEvent.drop(slot!, { dataTransfer });
		expect(callbacks.onMove).toHaveBeenCalledTimes(1);
		finish();
		await waitFor(() =>
			expect(view.getByRole('region', { name: 'Workflow canvas' })).toHaveProperty(
				'ariaBusy',
				'false'
			)
		);
	});

	it('keeps focused fields active and serializes consecutive edits', async () => {
		let finish: () => void = () => {};
		const saving = new Promise<void>((resolve) => {
			finish = resolve;
		});
		const onInputChange = vi
			.fn()
			.mockImplementationOnce(() => saving)
			.mockResolvedValue(undefined);
		const view = render(WorkflowCanvas, props({ onInputChange }));
		const input = view.getByLabelText('Title');
		input.focus();
		await fireEvent.change(input, { target: { value: 'First title' } });
		expect(document.activeElement).toBe(input);
		expect(input).toHaveProperty('disabled', false);
		await fireEvent.change(input, { target: { value: 'Second title' } });
		expect(onInputChange).toHaveBeenCalledTimes(1);
		finish();
		await waitFor(() => expect(onInputChange).toHaveBeenCalledTimes(2));
		expect(onInputChange).toHaveBeenNthCalledWith(1, 'title', 'title', { Literal: 'First title' });
		expect(onInputChange).toHaveBeenNthCalledWith(2, 'title', 'title', { Literal: 'Second title' });
		expect(document.activeElement).toBe(input);
		expect(input).toHaveProperty('value', 'Second title');
	});

	it('renders the catalog with a real keyboard add callback and native payload', async () => {
		const onChoose = vi.fn();
		const view = render(ActionCatalog, { items: catalog, onChoose });
		const button = view.getByRole('button', { name: 'Add Delay' });
		await fireEvent.click(button);
		expect(onChoose).toHaveBeenCalledWith(catalog[1]);
		const dataTransfer = transfer({ type: 'catalog', catalogId: 'unused' });
		await fireEvent.dragStart(button, { dataTransfer });
		expect(JSON.parse(dataTransfer.getData(WORKFLOW_DRAG_TYPE))).toEqual({
			type: 'catalog',
			catalogId: 'delay-action'
		});
	});

	it('rebases queued condition edits on the latest saved operands', async () => {
		let finish: () => void = () => {};
		const saving = new Promise<void>((resolve) => {
			finish = resolve;
		});
		let current = {
			...workflow,
			steps: [
				step('condition', {
					If: {
						condition: { Equal: [{ Literal: 1 }, { Literal: 2 }] },
						then_steps: [],
						else_steps: []
					}
				})
			]
		};
		let callCount = 0;
		const onEditStep = vi.fn(async (stepId: string, kind: StepKind): Promise<void> => {
			if (++callCount === 1) await saving;
			current = { ...current, steps: [step(stepId, kind)] };
			await view.rerender({ workflow: current });
		});
		const view = render(WorkflowCanvas, props({ workflow: current, onEditStep }));
		await fireEvent.change(view.getByLabelText('Value'), { target: { value: '11' } });
		await fireEvent.change(view.getByLabelText('Compared with'), { target: { value: '22' } });
		finish();
		await waitFor(() => expect(onEditStep).toHaveBeenCalledTimes(2));
		expect(onEditStep).toHaveBeenLastCalledWith('condition', {
			If: {
				condition: { Equal: [{ Literal: 11 }, { Literal: 22 }] },
				then_steps: [],
				else_steps: []
			}
		});
	});

	it('scrolls near a canvas edge during native drag and cancels on drag end', async () => {
		const frames: FrameRequestCallback[] = [];
		vi.spyOn(window, 'requestAnimationFrame').mockImplementation((callback) => {
			frames.push(callback);
			return frames.length;
		});
		const cancel = vi.spyOn(window, 'cancelAnimationFrame').mockImplementation(() => {});
		const view = render(WorkflowCanvas, props());
		const canvas = view.getByRole('region', { name: 'Workflow canvas' });
		vi.spyOn(canvas, 'getBoundingClientRect').mockReturnValue({
			top: 0,
			bottom: 300,
			height: 300
		} as DOMRect);
		const handle = view.getByRole('button', { name: 'Move Set stream title' });
		const dataTransfer = transfer({ type: 'step', workflowId: workflow.id, stepId: 'title' });
		await fireEvent.dragStart(handle, { dataTransfer });
		const event = new MouseEvent('dragover', { bubbles: true, cancelable: true, clientY: 299 });
		Object.defineProperty(event, 'dataTransfer', { value: dataTransfer });
		await fireEvent(canvas, event);
		expect(frames).toHaveLength(1);
		frames[0](0);
		expect(canvas.scrollTop).toBeGreaterThan(0);
		await fireEvent.dragEnd(handle);
		expect(cancel).toHaveBeenCalled();
	});
});
