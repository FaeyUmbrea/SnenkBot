import type {
	ConfigSchema,
	FailureCategory,
	FailureKind,
	RunOutcome,
	StepTrace
} from './contracts/index';

export const runOutcomeLabels: Record<RunOutcome['status'], string> = {
	running: 'Running',
	succeeded: 'Succeeded',
	stopped: 'Stopped',
	cancelled: 'Cancelled',
	failed: 'Failed',
	rejected: 'Not started',
	interrupted: 'Interrupted'
};

const failureCategories: Record<FailureCategory, string> = {
	definition: 'Workflow configuration',
	missing_value: 'Missing value',
	invalid_type: 'Value type',
	capability: 'Unavailable action',
	connector: 'Connection',
	action: 'Action',
	timeout: 'Time limit',
	input: 'User input',
	limit: 'Execution limit',
	other: 'Execution'
};

const stepFailures: Record<FailureKind, string> = {
	invalid_definition: 'Check this step’s configuration.',
	missing_value: 'A required value was unavailable.',
	invalid_type: 'A value had an unexpected type.',
	capability_unavailable: 'This action is unavailable.',
	connector_unavailable: 'The required connection was unavailable.',
	action: 'The action failed.',
	timeout: 'The action exceeded its time limit.',
	input: 'The requested input could not be collected.',
	loop_limit: 'The loop reached its execution limit.',
	step_limit: 'The workflow reached its step limit.',
	call_depth: 'The workflow reached its nesting limit.',
	called_workflow: 'The called workflow failed.',
	no_child_succeeded: 'None of the attempted actions succeeded.'
};

export function runResult(outcome: RunOutcome): string {
	return outcome.status === 'failed'
		? `Failed · ${failureCategories[outcome.category]}`
		: runOutcomeLabels[outcome.status];
}

export function formatDuration(milliseconds: number): string {
	return milliseconds < 1000
		? `${Math.round(milliseconds)} ms`
		: `${(milliseconds / 1000).toFixed(1)} s`;
}

export function traceTitle(
	trace: StepTrace,
	schemaTitles: ReadonlyMap<string, string>,
	workflowTitles: Readonly<Record<string, string>>
): string {
	const kind = trace.kind;
	switch (kind.kind) {
		case 'action':
			return schemaTitles.get(`${kind.capability}:${kind.version}`) ?? 'Unavailable action';
		case 'call':
			return `Run ${workflowTitles[kind.workflow_id] ?? 'unavailable workflow'}`;
		case 'set_variable':
			return 'Set value';
		case 'if':
			return 'If';
		case 'while':
			return 'Repeat while';
		case 'one_or_more':
			return 'Try actions';
		case 'delay':
			return 'Wait';
		case 'request_input':
			return 'Ask for input';
		case 'stop':
			return 'Stop workflow';
	}
}

export function traceFailure(trace: StepTrace): string | null {
	if (trace.outcome.status !== 'failed') return null;
	const failure = trace.outcome;
	return [
		stepFailures[failure.kind],
		failure.remote_effect_uncertain
			? 'The remote change may have happened. Check before retrying.'
			: '',
		failure.continued_by_policy ? 'The workflow continued after this failure.' : ''
	]
		.filter(Boolean)
		.join(' ');
}

export function schemaTitleIndex(schemas: readonly ConfigSchema[]): ReadonlyMap<string, string> {
	return new Map(schemas.map((schema) => [`${schema.id}:${schema.version}`, schema.title]));
}
