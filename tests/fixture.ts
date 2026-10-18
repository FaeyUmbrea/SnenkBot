import type { ConfigSchema, Step, StepKind, Workflow } from '../src/lib/contracts/index';
import type { CatalogItem } from '../src/lib/canvas';

export const schemas: ConfigSchema[] = [
	{
		id: 'obs.set_title',
		version: 1,
		title: 'Set stream title',
		fields: [
			{
				id: 'title',
				label: 'Title',
				description: '',
				introduced_in: 1,
				kind: 'text',
				required: true,
				choice_source: null
			}
		],
		outputs: [{ id: 'title', label: 'Stream title', description: '', kind: 'text', required: true }]
	},
	{
		id: 'obs.set_game',
		version: 1,
		title: 'Set game',
		fields: [
			{
				id: 'game',
				label: 'Game',
				description: '',
				introduced_in: 1,
				kind: 'text',
				required: true,
				choice_source: null
			},
			{
				id: 'enabled',
				label: 'Update category',
				description: '',
				introduced_in: 1,
				kind: 'toggle',
				required: true,
				choice_source: null
			}
		],
		outputs: [{ id: 'game', label: 'Game name', description: '', kind: 'text', required: true }]
	},
	{
		id: 'chat.send',
		version: 1,
		title: 'Send message',
		fields: [
			{
				id: 'message',
				label: 'Message',
				description: '',
				introduced_in: 1,
				kind: 'text',
				required: true,
				choice_source: null
			}
		],
		outputs: []
	}
];

export function step(id: string, kind: StepKind): Step {
	return { id, on_failure: 'Stop', kind };
}

export const workflow: Workflow = {
	id: 'fixture-workflow',
	revision: 1,
	overlap: false,
	outputs: {},
	steps: [
		step('title', {
			Action: {
				capability: 'obs.set_title',
				version: 1,
				inputs: { title: { Literal: 'Sunday stories with Faey' } },
				deadline_ms: null
			}
		}),
		step('game', {
			Action: {
				capability: 'obs.set_game',
				version: 1,
				inputs: { game: { Literal: 'Guild Wars 2' }, enabled: { Literal: true } },
				deadline_ms: null
			}
		}),
		step('if', {
			If: {
				condition: { Equal: [{ Trigger: { name: 'is_live', fallback: null } }, { Literal: true }] },
				then_steps: [
					step('then-message', {
						Action: {
							capability: 'chat.send',
							version: 1,
							inputs: {
								message: {
									Text: [
										{ Literal: 'Now playing: ' },
										{ Value: { Reference: { step_id: 'game', output_id: 'game', fallback: null } } }
									]
								}
							},
							deadline_ms: null
						}
					}),
					step('nested-if', {
						If: {
							condition: {
								Greater: [{ Variable: { name: 'viewers', fallback: null } }, { Literal: 20 }]
							},
							then_steps: [step('delay', { Delay: { millis: 1250 } })],
							else_steps: [
								step('call', { Call: { workflow_id: 'announce', binding: 'AllNamedArguments' } })
							]
						}
					})
				],
				else_steps: [
					step('else-message', {
						Action: {
							capability: 'chat.send',
							version: 1,
							inputs: { message: { Literal: 'See you soon!' } },
							deadline_ms: null
						}
					})
				]
			}
		}),
		step('name', {
			SetVariable: {
				name: 'stream title',
				value: { Reference: { step_id: 'title', output_id: 'title', fallback: null } }
			}
		})
	]
};

export const workflowTitles = { announce: 'Announce the stream' };
export const catalog: CatalogItem[] = [
	{
		id: 'title-action',
		title: 'Set stream title',
		category: 'Twitch',
		kind: {
			Action: {
				capability: 'obs.set_title',
				version: 1,
				inputs: { title: { Literal: '' } },
				deadline_ms: null
			}
		}
	},
	{ id: 'delay-action', title: 'Delay', category: 'Workflow', kind: { Delay: { millis: 1000 } } }
];
