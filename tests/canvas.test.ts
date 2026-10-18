import { describe, expect, it } from 'vitest';
import { createWorkflowIndex, referenceTitle, validPlacement } from '../src/lib/canvas';
import { catalog, schemas, workflow, workflowTitles } from './fixture';
import type { StepDestination, StepPosition } from '../src/lib/contracts/index';

const before = (step_id: string): StepPosition => ({ Before: { step_id } });
const branch = (parent_id: string, name: 'Then' | 'Else' | 'Body' = 'Then'): StepDestination => ({
	Branch: { parent_id, branch: name }
});

describe('workflow placement', () => {
	const index = createWorkflowIndex(workflow, schemas, workflowTitles);
	for (const [description, stepId, destination, position, expected] of [
		['move root into then', 'title', branch('if'), before('then-message'), true],
		['move then into else', 'then-message', branch('if', 'Else'), 'Append', true],
		['move nested action to root', 'delay', 'Root', before('title'), true],
		['reject parent into itself', 'if', branch('if'), 'Append', false],
		['reject parent into descendant', 'if', branch('nested-if'), 'Append', false],
		['reject another list anchor', 'title', branch('if'), before('game'), false],
		['reject missing parent', 'title', branch('absent'), 'Append', false],
		['reject missing step', 'absent', 'Root', 'Append', false],
		['reject same position', 'title', 'Root', before('title'), false],
		['reject next sibling no-op', 'title', 'Root', before('game'), false],
		['reject append no-op', 'name', 'Root', 'Append', false],
		['accept add before root step', undefined, 'Root', before('game'), true],
		['reject add into action', undefined, branch('title'), 'Append', false]
	] as const) {
		it(description, () => {
			expect(validPlacement(workflow, destination, position, stepId, index)).toBe(expected);
			expect(validPlacement(workflow, destination, position, stepId)).toBe(expected);
		});
	}

	it('resolves producer and output display titles through the snapshot index', () => {
		expect(
			referenceTitle(
				{ Reference: { step_id: 'game', output_id: 'game', fallback: null } },
				workflow.steps,
				schemas,
				workflowTitles,
				index
			)
		).toBe('Set game · Game name');
		expect(catalog[0].kind).toHaveProperty('Action');
	});
});
