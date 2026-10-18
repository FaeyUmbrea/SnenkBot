export const WORKFLOW_DRAG_TYPE = 'application/x-snenkbot-workflow';

export type WorkflowDrag =
	{ type: 'step'; workflowId: string; stepId: string } | { type: 'catalog'; catalogId: string };

let activeDrag: WorkflowDrag | undefined;

export function startWorkflowDrag(event: DragEvent, payload: WorkflowDrag): void {
	if (!event.dataTransfer) return;
	activeDrag = payload;
	event.dataTransfer.setData(WORKFLOW_DRAG_TYPE, JSON.stringify(payload));
	event.dataTransfer.effectAllowed = payload.type === 'step' ? 'move' : 'copy';
}

export function currentWorkflowDrag(): WorkflowDrag | undefined {
	return activeDrag;
}

export function endWorkflowDrag(): void {
	activeDrag = undefined;
}

export function readWorkflowDrag(event: DragEvent): WorkflowDrag | undefined {
	try {
		const payload: unknown = JSON.parse(event.dataTransfer?.getData(WORKFLOW_DRAG_TYPE) ?? '');
		if (!payload || typeof payload !== 'object') return undefined;
		if (
			'type' in payload &&
			payload.type === 'step' &&
			'workflowId' in payload &&
			typeof payload.workflowId === 'string' &&
			'stepId' in payload &&
			typeof payload.stepId === 'string'
		) {
			return { type: 'step', workflowId: payload.workflowId, stepId: payload.stepId };
		}
		if (
			'type' in payload &&
			payload.type === 'catalog' &&
			'catalogId' in payload &&
			typeof payload.catalogId === 'string'
		) {
			return { type: 'catalog', catalogId: payload.catalogId };
		}
	} catch {
		return undefined;
	}
	return undefined;
}
