import js from "@eslint/js";
import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";
import globals from "globals";

export default tseslint.config(
  // Glaze's own code (ADR-016) is linted by its rules: glaze.eslint.config.mjs.
  {
    ignores: [
      "dist",
      "storybook-static",
      "src-tauri",
      "node_modules",
      "src/components/glaze",
      "src/lib/glaze",
      "glaze.eslint.config.mjs",
    ],
  },
  js.configs.recommended,
  ...tseslint.configs.strict,
  {
    files: ["**/*.{ts,tsx}"],
    languageOptions: { globals: globals.browser },
    plugins: { "react-hooks": reactHooks },
    rules: {
      ...reactHooks.configs.recommended.rules,
      // AGENTS.md: strict mode, no `any`.
      "@typescript-eslint/no-explicit-any": "error",
    },
  },
);
