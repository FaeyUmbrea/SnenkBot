import js from '@eslint/js';
import svelte from 'eslint-plugin-svelte';
import ts from 'typescript-eslint';
import globals from 'globals';

export default ts.config(
	{ ignores: ['dist/**', 'node_modules/**', 'src/lib/contracts/**'] },
	js.configs.recommended,
	...ts.configs.recommended,
	...svelte.configs['flat/recommended'],
	{ languageOptions: { globals: { ...globals.browser, ...globals.node } } },
	{ files: ['**/*.svelte'], languageOptions: { parserOptions: { parser: ts.parser } } }
);
