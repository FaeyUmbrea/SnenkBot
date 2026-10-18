import { defineConfig } from 'vitest/config';
import { svelte } from '@sveltejs/vite-plugin-svelte';

export default defineConfig({
	plugins: [svelte()],
	server: {
		host: '127.0.0.1',
		port: 1420,
		strictPort: true
	},
	resolve: { conditions: ['browser'] },
	test: {
		environment: 'jsdom',
		include: ['tests/**/*.test.ts'],
		setupFiles: ['tests/setup.ts'],
		maxWorkers: 2,
		restoreMocks: true
	}
});
