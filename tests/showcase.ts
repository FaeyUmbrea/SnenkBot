import type { ConfigSchema, Workflow } from '../src/lib/contracts/index';
import { step } from './fixture';

// Fictional data for public screenshots; these use the production chat action's field vocabulary.
export const schemas: ConfigSchema[] = [
	{
		id: 'twitch.send_chat',
		version: 1,
		title: 'Send chat message',
		fields: [
			{
				id: 'message',
				label: 'Message',
				description: "Text sent to the broadcaster's chat",
				introduced_in: 1,
				kind: 'text',
				required: true,
				choice_source: null
			},
			{
				id: 'broadcaster_fallback',
				label: 'Use broadcaster if bot authorization fails',
				description: 'Send as the broadcaster only when bot authorization fails before sending',
				introduced_in: 1,
				kind: 'toggle',
				required: false,
				choice_source: null
			}
		],
		outputs: [
			{ id: 'message_id', label: 'Message ID', description: '', kind: 'text', required: true }
		]
	}
];

export const workflow: Workflow = {
	id: 'showcase-welcome',
	revision: 1,
	overlap: false,
	outputs: {},
	steps: [
		step('hello', {
			Action: {
				capability: 'twitch.send_chat',
				version: 1,
				inputs: {
					message: { Literal: 'Welcome in! Get comfy and enjoy the stream.' },
					broadcaster_fallback: { Literal: null }
				},
				deadline_ms: null
			}
		}),
		step('pause', { Delay: { millis: 2000 } }),
		step('links', {
			Action: {
				capability: 'twitch.send_chat',
				version: 1,
				inputs: {
					message: { Literal: 'Use !commands to see what you can do in chat.' },
					broadcaster_fallback: { Literal: null }
				},
				deadline_ms: null
			}
		})
	]
};
