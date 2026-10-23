import js from '@eslint/js'
import jsxA11y from 'eslint-plugin-jsx-a11y'
import reactHooks from 'eslint-plugin-react-hooks'
import globals from 'globals'
import tseslint from 'typescript-eslint'

export default tseslint.config(
  {
    ignores: [
      'dist',
      'dist-mock',
      'node_modules',
      'playwright-report',
      'test-results',
      'coverage',
      'src/api/schema.d.ts',
      'src/routeTree.gen.ts',
      'src/mocks/generated',
      'e2e/screenshots',
    ],
  },
  js.configs.recommended,
  ...tseslint.configs.recommendedTypeChecked,
  jsxA11y.flatConfigs.recommended,
  {
    languageOptions: {
      globals: { ...globals.browser },
      parserOptions: { projectService: true, tsconfigRootDir: import.meta.dirname },
    },
    plugins: { 'react-hooks': reactHooks },
    rules: {
      'react-hooks/rules-of-hooks': 'error',
      'react-hooks/exhaustive-deps': 'error',
      '@typescript-eslint/consistent-type-imports': ['error', { fixStyle: 'inline-type-imports' }],
      // A floating promise is a swallowed error: await it, return it, or `void` it on purpose.
      '@typescript-eslint/no-floating-promises': 'error',
      '@typescript-eslint/no-misused-promises': [
        'error',
        { checksVoidReturn: { attributes: false } },
      ],
      '@typescript-eslint/no-unused-vars': [
        'error',
        { argsIgnorePattern: '^_', varsIgnorePattern: '^_' },
      ],
      // Raw hex in components defeats theming and the contrast proof: use token classes.
      'no-restricted-syntax': [
        'error',
        {
          selector: 'Literal[value=/^#[0-9a-fA-F]{3,8}$/]',
          message:
            'Use a design token (var(--color-*) or a Tailwind token class), not a raw hex color.',
        },
      ],
    },
  },
  {
    // Plain node scripts and config files are not part of the type-checked program.
    files: ['**/*.mjs', '**/*.js', 'eslint.config.js'],
    ...tseslint.configs.disableTypeChecked,
    languageOptions: {
      ...tseslint.configs.disableTypeChecked.languageOptions,
      globals: { ...globals.node, ...globals.browser },
    },
  },
  {
    files: ['vite.config.ts', 'vitest.config.ts', 'playwright.config.ts', 'e2e/**/*.ts'],
    languageOptions: { globals: { ...globals.node } },
  },
  {
    // TanStack Router redirects and not-found answers are thrown objects by design
    // (`throw redirect(...)`, `throw notFound()`), not Error instances.
    files: ['src/routes/**'],
    rules: { '@typescript-eslint/only-throw-error': 'off' },
  },
  {
    files: ['**/*.test.{ts,tsx}', 'src/test/**', 'e2e/**'],
    rules: {
      '@typescript-eslint/no-non-null-assertion': 'off',
      '@typescript-eslint/unbound-method': 'off',
      'no-restricted-syntax': 'off',
    },
  },
  {
    // The demo page deliberately shows raw examples; mocks are fixtures with literal data.
    files: ['src/mocks/**', 'src/features/demo/**', 'scripts/**'],
    rules: { 'no-restricted-syntax': 'off' },
  },
)
