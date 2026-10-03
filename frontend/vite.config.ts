import { defineConfig } from 'vitest/config';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { fileURLToPath } from 'node:url';

export default defineConfig({
	plugins: [svelte()],
	server: { fs: { allow: [fileURLToPath(new URL('..', import.meta.url))] } },
	resolve: { conditions: ['browser'] },
	test: {
		environment: 'jsdom',
		include: ['tests/**/*.test.ts'],
		setupFiles: ['tests/setup.ts'],
		maxWorkers: 2,
		restoreMocks: true
	}
});
