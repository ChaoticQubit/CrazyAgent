# Vendored assets

Downloaded once on 2026-10-23. No npm dependency was added. Files are served from the app bundle, so the CSP keeps `font-src 'self'` and `img-src 'self' data:`.

## Fonts (`fonts/`, SIL Open Font License 1.1)

| File | Source | Version | SHA-256 |
|---|---|---|---|
| `InstrumentSans.woff2` (variable, `wdth` 75-100, `wght` 400-700) | https://raw.githubusercontent.com/Instrument/instrument-sans/7fa22308a3d0c94ee2b3cd537a1196b65db34a3e/fonts/webfonts/InstrumentSans%5Bwdth,wght%5D.woff2 | upstream commit `7fa22308`, the commit Google Fonts pins in `ofl/instrumentsans/METADATA.pb` | `aa72922aafcc0dc18f36ec1d805b0212057dabe8b9d5b8b57f67035aea1b826d` |
| `JetBrainsMono.woff2` (variable, `wght` 100-800) | https://raw.githubusercontent.com/JetBrains/JetBrainsMono/19371302b95d218af43299bce79ddbddd0bc364d/fonts/webfonts/JetBrainsMono%5Bwght%5D.woff2 | upstream commit `19371302` (font version 2.304), the commit Google Fonts pins in `ofl/jetbrainsmono/METADATA.pb` | `31ec365b93e4bad6f202ce23352a56d01ca4462b2afc782ed2cf6fa42ca9ac0e` |
| `InstrumentSans-OFL.txt` | https://raw.githubusercontent.com/google/fonts/main/ofl/instrumentsans/OFL.txt | google/fonts `main` | |
| `JetBrainsMono-OFL.txt` | https://raw.githubusercontent.com/google/fonts/main/ofl/jetbrainsmono/OFL.txt | google/fonts `main` | |

Google Fonts' own repository (`google/fonts`) ships only TTF files for these families. The woff2 files come from the upstream repositories that Google Fonts builds from, at the exact commits its metadata pins.

## Icons (`icons/`, ISC license, `icons/LICENSE`)

Lucide 1.61.0, from `https://raw.githubusercontent.com/lucide-icons/lucide/1.61.0/icons/<name>.svg` and `https://raw.githubusercontent.com/lucide-icons/lucide/1.61.0/LICENSE`. Every file is parsed by `parseSvg` in `src/ui/icon.tsx`, which accepts only basic shapes with geometry attributes.

## Provider and harness logos (`logos/`, CC0 1.0, `logos/LICENSE.md`)

Simple Icons 16.35.0, from `https://raw.githubusercontent.com/simple-icons/simple-icons/16.35.0/icons/<slug>.svg`: anthropic, claude, cursor, githubcopilot, google, googlegemini, ollama, opencode, openrouter, qwen, vercel. Simple Icons has no OpenAI, Aider, Goose or Kiro mark at this version; those use a monogram. Logos are drawn in the current text color and only identify a provider.

## Not generated yet

The app icon and favicon wait for the owner's brand choice (owner instruction, 2026-10-23). `src-tauri/icons/icon.png` is unchanged. Generate both from the chosen mark with `node node_modules/@tauri-apps/cli/tauri.js icon <source>` and reference the favicon in `index.html`.
