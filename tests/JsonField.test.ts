import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import JsonField from '../src/lib/JsonField.svelte';
import InputControl from '../src/lib/InputControl.svelte';

afterEach(cleanup);

describe('shared JSON input', () => {
	it('retains incomplete input without replacing the configured value', async () => {
		const onChange = vi.fn();
		const view = render(JsonField, { label: 'Payload', value: { count: 1 }, onChange });
		const input = view.getByLabelText('Payload');
		await fireEvent.change(input, { target: { value: '{"count":' } });
		expect(onChange).not.toHaveBeenCalled();
		expect(view.getByRole('alert').textContent).toBe('Enter valid JSON.');
		expect(input.getAttribute('aria-invalid')).toBe('true');
		expect(input.getAttribute('aria-describedby')).toBe(view.getByRole('alert').id);
		expect((input as HTMLTextAreaElement).checkValidity()).toBe(false);
		await fireEvent.change(input, { target: { value: '{"count":2,"items":[true,null]}' } });
		expect(onChange).toHaveBeenCalledExactlyOnceWith({ count: 2, items: [true, null] });
		expect(view.queryByRole('alert')).toBeNull();
		expect((input as HTMLTextAreaElement).checkValidity()).toBe(true);
	});

	it('edits structured action literals without flattening them into text', async () => {
		const onChange = vi.fn();
		const view = render(InputControl, {
			label: 'Payload',
			kind: 'json',
			input: { Literal: { original: true } },
			steps: [],
			schemas: [],
			onChange,
			onSourcePicker: vi.fn()
		});
		await fireEvent.change(view.getByLabelText('Payload'), { target: { value: 'null' } });
		expect(onChange).toHaveBeenCalledExactlyOnceWith({ Literal: null });
	});
	it('replaces stale invalid drafts when the owning form loads another value', async () => {
		const onChange = vi.fn();
		const view = render(JsonField, { label: 'Payload', value: { old: true }, onChange });
		await fireEvent.change(view.getByLabelText('Payload'), { target: { value: '{' } });
		await view.rerender({ label: 'Payload', value: null, onChange });
		expect(view.queryByRole('alert')).toBeNull();
		expect(view.getByLabelText('Payload')).toHaveProperty('value', 'null');
		expect((view.getByLabelText('Payload') as HTMLTextAreaElement).checkValidity()).toBe(true);
		expect(onChange).not.toHaveBeenCalled();
	});
});
