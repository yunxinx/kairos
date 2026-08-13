import js from '@eslint/js';
import eslintConfigPrettier from 'eslint-config-prettier';
import pluginVue from 'eslint-plugin-vue';
import pluginVueA11y from 'eslint-plugin-vuejs-accessibility';
import tseslint from 'typescript-eslint';
import vueParser from 'vue-eslint-parser';

export default tseslint.config(
  {
    ignores: [
      'dist',
      'node_modules',
      'src/routeTree.gen.ts',
      'eslint.config.js',
      'playwright.config.ts',
      'e2e/**',
    ],
  },
  js.configs.recommended,
  ...tseslint.configs.strictTypeChecked,
  ...pluginVue.configs['flat/recommended'],
  ...pluginVueA11y.configs['flat/recommended'],
  eslintConfigPrettier,
  {
    files: ['**/*.{ts,vue}'],
    languageOptions: {
      parser: vueParser,
      parserOptions: {
        parser: tseslint.parser,
        projectService: true,
        extraFileExtensions: ['.vue'],
      },
    },
    rules: {
      'vue/multi-word-component-names': 'off',
      'vue/block-lang': ['error', { script: { lang: 'ts' } }],
      '@typescript-eslint/no-confusing-void-expression': 'off',
      '@typescript-eslint/restrict-template-expressions': 'off',
      'vuejs-accessibility/label-has-for': [
        'error',
        {
          controlComponents: ['FormTextInput', 'FormPasswordInput', 'FormCheckbox'],
          required: { every: ['id'] },
        },
      ],
    },
  },
  {
    files: ['src/routes/**/*.{ts,vue}', 'src/router.ts', 'src/lib/router-guards.ts'],
    rules: {
      '@typescript-eslint/no-unsafe-assignment': 'off',
      '@typescript-eslint/only-throw-error': 'off',
    },
  },
  {
    files: ['src/app/shell/NavBar.vue', 'src/app/shell/MobileNav.vue'],
    rules: {
      'vue/attribute-hyphenation': 'off',
    },
  },
  {
    files: ['src/components/ui/FormField.vue'],
    rules: {
      // 控件在 slot 内且带与 inputId 一致的 id；规则无法跨 slot 静态关联 label[for]。
      'vuejs-accessibility/label-has-for': 'off',
    },
  },
);
