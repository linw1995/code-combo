# Theme Notes

- Files:
  - `catppuccin_scheme.json`: UI and tree-sitter styles (fg/bg/modifiers).
  - `catppuccin_mocha_palette.json`: palette name to hex mapping.
- Adding a new UI style key:
  1. Add the key under `ui` in `catppuccin_scheme.json`.
  2. Add the field to `FinalizedUiTheme` and `build_theme!` in `src/theme.rs`.
  3. Use `global::theme().ui.<key>` in components.
- Prefer bg-only styles for component backgrounds; keep fg separate unless required.
