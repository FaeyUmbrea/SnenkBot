import { afterEach } from 'vitest';
import { cleanup } from '@testing-library/svelte';
import { endWorkflowDrag } from '../src/lib/drag';

afterEach(() => {
	cleanup();
	endWorkflowDrag();
});

if (!globalThis.CSS) Object.defineProperty(globalThis, 'CSS', { value: {} });
if (!CSS.escape) CSS.escape = (value) => value.replace(/[^\w-]/g, (character) => `\\${character}`);
