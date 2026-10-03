import type {
	ConfigSchema,
	Condition,
	Input,
	Step,
	StepBranch,
	StepDestination,
	StepKind,
	StepPosition,
	Workflow
} from './contracts/index';

export type CanvasEffect = void | Promise<void>;
export type ConditionUpdate = (current: Condition) => Condition;

export interface CatalogItem {
	id: string;
	title: string;
	category: string;
	description?: string;
	kind: StepKind;
}

export interface CanvasProps {
	workflow: Workflow;
	schemas: readonly ConfigSchema[];
	workflowTitles?: Readonly<Record<string, string>>;
	catalog?: readonly CatalogItem[];
	summaryFields?: Readonly<Record<string, readonly string[]>>;
	selectedStepId?: string | null;
	disabled?: boolean;
	onSelect: (stepId: string | null) => void;
	onMove: (stepId: string, destination: StepDestination, position: StepPosition) => CanvasEffect;
	onAdd: (kind: StepKind, destination: StepDestination, position: StepPosition) => CanvasEffect;
	onInputChange: (stepId: string, fieldId: string, value: Input) => CanvasEffect;
	onEditStep: (stepId: string, kind: StepKind) => CanvasEffect;
	onSourcePicker: (stepId: string, fieldId: string, current: Input) => void;
	onInspectStep?: (stepId: string) => void;
}

export function actionSummaryFields(
	step: Step,
	schemas: readonly ConfigSchema[],
	summaryFields: Readonly<Record<string, readonly string[]>>
): string[] {
	if (typeof step.kind === 'string' || !step.kind.Action) return [];
	const action = step.kind.Action;
	const explicit = summaryFields[action.capability];
	if (explicit) return [...explicit];
	const required = schemaFor(step, schemas)?.fields.find((field) => field.required);
	return required ? [required.id] : Object.keys(action.inputs).slice(0, 1);
}

export interface StepList {
	destination: StepDestination;
	steps: readonly Step[];
	title: string;
}

export interface WorkflowIndex {
	steps: ReadonlyMap<string, Step>;
	lists: readonly StepList[];
	destinations: ReadonlyMap<string, StepList>;
	locations: ReadonlyMap<string, { destination: StepDestination; index: number }>;
	ancestors: ReadonlyMap<string, ReadonlySet<string>>;
}

export function destinationKey(destination: StepDestination): string {
	return destination === 'Root'
		? 'root'
		: `${destination.Branch.parent_id}:${destination.Branch.branch}`;
}

export function createWorkflowIndex(
	workflow: Workflow,
	schemas: readonly ConfigSchema[],
	workflowTitles: Readonly<Record<string, string>> = {}
): WorkflowIndex {
	const steps = new Map<string, Step>();
	const lists: StepList[] = [];
	const destinations = new Map<string, StepList>();
	const locations = new Map<string, { destination: StepDestination; index: number }>();
	const ancestors = new Map<string, ReadonlySet<string>>();
	function visit(list: StepList, parents: readonly string[]) {
		lists.push(list);
		destinations.set(destinationKey(list.destination), list);
		list.steps.forEach((step, index) => {
			steps.set(step.id, step);
			locations.set(step.id, { destination: list.destination, index });
			ancestors.set(step.id, new Set(parents));
			for (const child of childLists(step)) {
				const label =
					child.branch === 'Then'
						? 'If actions'
						: child.branch === 'Else'
							? 'Otherwise actions'
							: 'Inside';
				visit(
					{
						destination: { Branch: { parent_id: step.id, branch: child.branch } },
						steps: child.steps,
						title: `${stepTitle(step, schemas, workflowTitles)} · ${label}`
					},
					[...parents, step.id]
				);
			}
		});
	}
	visit({ destination: 'Root', steps: workflow.steps, title: 'Workflow' }, []);
	return { steps, lists, destinations, locations, ancestors };
}

export function childLists(step: Step): { branch: StepBranch; steps: readonly Step[] }[] {
	const kind = step.kind;
	if (typeof kind === 'string') return [];
	if (kind.If !== undefined)
		return [
			{ branch: 'Then', steps: kind.If.then_steps },
			{ branch: 'Else', steps: kind.If.else_steps }
		];
	if (kind.While !== undefined) return [{ branch: 'Body', steps: kind.While.steps }];
	if (kind.OneOrMore !== undefined) return [{ branch: 'Body', steps: kind.OneOrMore.steps }];
	return [];
}

export function allSteps(steps: readonly Step[]): Step[] {
	return steps.flatMap((step) => [
		step,
		...childLists(step).flatMap((list) => allSteps(list.steps))
	]);
}

export function schemaFor(step: Step, schemas: readonly ConfigSchema[]): ConfigSchema | undefined {
	if (typeof step.kind === 'string' || step.kind.Action === undefined) return undefined;
	const action = step.kind.Action;
	return schemas.find(
		(schema) => schema.id === action.capability && schema.version === action.version
	);
}

export function stepTitle(
	step: Step,
	schemas: readonly ConfigSchema[],
	workflowTitles: Readonly<Record<string, string>> = {}
): string {
	const kind = step.kind;
	if (typeof kind === 'string') return 'Stop workflow';
	if (kind.Action !== undefined) return schemaFor(step, schemas)?.title ?? 'Action unavailable';
	if (kind.SetVariable !== undefined) return 'Set variable';
	if (kind.If !== undefined) return 'If';
	if (kind.While !== undefined) return 'While';
	if (kind.OneOrMore !== undefined) return 'One or more';
	if (kind.Delay !== undefined) return 'Delay';
	if (kind.RequestInput !== undefined) return kind.RequestInput.title || 'Request input';
	return `Run ${workflowTitles[kind.Call.workflow_id] ?? 'workflow'}`;
}

export function fieldTitle(id: string): string {
	if (/^[a-f0-9]{8}-[a-f0-9-]{27}$/i.test(id)) return 'Value';
	const title = id.replace(/[_-]+/g, ' ');
	return title.charAt(0).toUpperCase() + title.slice(1);
}

export function referenceTitle(
	input: Input,
	steps: readonly Step[],
	schemas: readonly ConfigSchema[],
	workflowTitles: Readonly<Record<string, string>> = {},
	index?: WorkflowIndex
): string {
	if (input.Reference !== undefined) {
		const reference = input.Reference;
		const producer = index
			? index.steps.get(reference.step_id)
			: allSteps(steps).find((step) => step.id === reference.step_id);
		if (!producer) return 'Missing action · Output';
		let output = schemaFor(producer, schemas)?.outputs.find(
			(output) => output.id === reference.output_id
		)?.label;
		if (typeof producer.kind !== 'string' && producer.kind.RequestInput !== undefined) {
			output =
				producer.kind.RequestInput.fields.find((field) => field.id === reference.output_id)
					?.label ?? undefined;
		}
		return `${stepTitle(producer, schemas, workflowTitles)} · ${output ?? fieldTitle(reference.output_id)}`;
	}
	if (input.Variable !== undefined) return `Variable · ${input.Variable.name}`;
	if (input.Trigger !== undefined) return `Trigger · ${fieldTitle(input.Trigger.name)}`;
	if (input.Object !== undefined) return `Object · ${Object.keys(input.Object).length} fields`;
	if (input.Array !== undefined) return `List · ${input.Array.length} items`;
	if (Array.isArray(input.Literal)) return `List · ${input.Literal.length} items`;
	if (input.Literal !== null && typeof input.Literal === 'object')
		return `Object · ${Object.keys(input.Literal).length} fields`;
	return 'Text with values';
}

export function stepLists(
	workflow: Workflow,
	schemas: readonly ConfigSchema[],
	workflowTitles: Readonly<Record<string, string>> = {}
): StepList[] {
	const lists: StepList[] = [{ destination: 'Root', steps: workflow.steps, title: 'Workflow' }];
	for (const step of allSteps(workflow.steps)) {
		for (const list of childLists(step)) {
			const label =
				list.branch === 'Then'
					? 'If actions'
					: list.branch === 'Else'
						? 'Otherwise actions'
						: 'Inside';
			lists.push({
				destination: { Branch: { parent_id: step.id, branch: list.branch } },
				steps: list.steps,
				title: `${stepTitle(step, schemas, workflowTitles)} · ${label}`
			});
		}
	}
	return lists;
}

export function sameDestination(left: StepDestination, right: StepDestination): boolean {
	if (typeof left === 'string' || typeof right === 'string') return left === right;
	return (
		left.Branch.parent_id === right.Branch.parent_id && left.Branch.branch === right.Branch.branch
	);
}

export function listAt(
	workflow: Workflow,
	destination: StepDestination
): readonly Step[] | undefined {
	if (destination === 'Root') return workflow.steps;
	const parent = allSteps(workflow.steps).find((step) => step.id === destination.Branch.parent_id);
	return (
		parent && childLists(parent).find((list) => list.branch === destination.Branch.branch)?.steps
	);
}

export function validPlacement(
	workflow: Workflow,
	destination: StepDestination,
	position: StepPosition,
	stepId?: string,
	index?: WorkflowIndex
): boolean {
	const destinationSteps = index
		? index.destinations.get(destinationKey(destination))?.steps
		: listAt(workflow, destination);
	if (!destinationSteps) return false;
	if (typeof position !== 'string') {
		const targetLocation = index?.locations.get(position.Before.step_id);
		if (
			index
				? !targetLocation || !sameDestination(targetLocation.destination, destination)
				: !destinationSteps.some((step) => step.id === position.Before.step_id)
		)
			return false;
	}
	if (!stepId) return true;
	const moving = index
		? index.steps.get(stepId)
		: allSteps(workflow.steps).find((step) => step.id === stepId);
	if (!moving) return false;
	if (typeof destination !== 'string') {
		const parent = destination.Branch.parent_id;
		if (
			index
				? parent === stepId || index.ancestors.get(parent)?.has(stepId)
				: allSteps([moving]).some((step) => step.id === parent)
		)
			return false;
	}
	const sourceLocation = index?.locations.get(stepId);
	const sourceIndex = index
		? sourceLocation && sameDestination(sourceLocation.destination, destination)
			? sourceLocation.index
			: -1
		: destinationSteps.findIndex((step) => step.id === stepId);
	if (sourceIndex < 0) return true;
	if (position === 'Append') return sourceIndex !== destinationSteps.length - 1;
	const targetIndex = index
		? index.locations.get(position.Before.step_id)?.index
		: destinationSteps.findIndex((step) => step.id === position.Before.step_id);
	return targetIndex !== sourceIndex && targetIndex !== sourceIndex + 1;
}
