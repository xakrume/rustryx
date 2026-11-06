# Web UI design brief

Source of truth for the UI work (wave 3). Generated with the ui-ux-pro-max skill
(`design-system/rustryx/MASTER.md`, dials: variance 3, motion 3, density 8) and
reviewed by hand. If this brief and MASTER.md disagree, this brief wins.

## Decisions

| Topic | Decision | Reason |
|---|---|---|
| Stack | React 19, TypeScript, Vite, Tailwind, shadcn/ui (Radix), TanStack Router, TanStack Query, TanStack Table, Recharts | The original product uses React; shadcn gives accessible table, command palette, sidebar, dialogs; no stack was detectable in the repo, so this is a proposal the maintainers can veto before wave 3 starts |
| Delivery | `ui/` builds to static files embedded into the `rustryx` binary (SPA fallback, hashed assets, strict CSP, no inline scripts) | one artifact, no separate web server, works behind any ingress |
| API contract | UI talks only to the native API `/api/v1/*` described by `docs/openapi/rustryx-api-v1.yaml`; the TypeScript client is generated from it | backend and UI are built in parallel against one contract |
| Auth in UI | Keycloak code flow with PKCE through the server (cookie session, SameSite=Lax, CSRF token on mutations); local login form for break-glass; no tokens in localStorage | README 8.3 |
| Style | Dark and light themes, density 8 (dashboard), motion 150 to 300 ms | a tool people sit in for a long time |
| Palette | Quiet neutrals (`#f5f5f7` page, white cards, `#1d1d1f` ink; true black and graphite in dark), one action blue `#0071e3`, green only for success, red for destruction; both themes defined as semantic tokens (`--color-*`), never raw hex in components | every pair is proven by `scripts/check-contrast.mjs` |
| Type | the system face (SF Pro, Segoe UI, self-hosted Fira Sans as fallback) for UI text and headings; the system monospace (SF Mono, Menlo) then Fira Code for coordinates, digests, paths, commands, logs | code font is limited to technical values; headings use tight negative tracking |
| Icons | Lucide SVG only, no emoji; icon-only buttons carry an accessible label | skill rule |

## Rules every screen must satisfy

1. Accessibility first: text contrast 4.5:1 in BOTH themes (validate tokens with a script in CI), visible focus ring on every control, full keyboard operation, skip-to-content link, `prefers-reduced-motion` respected, status never by color alone (icon or text too).
2. Tables (the main component): server-side sort, filter and keyset pagination, column visibility, sticky header, row selection with a bulk action bar, horizontal scroll fallback on narrow screens, empty/loading/error states, density toggle (compact default).
3. Repository browse is a keyboard-navigable tree (arrow keys, Home/End, type-ahead) with a detail pane for the selected asset: path, size, checksums, blob store, upload time, download count, copy-to-clipboard buttons, `client setup` panel per format (mvn, npm, pip, docker, helm, dart pub).
4. Destructive actions (delete repository, component, tag, role, token) need a confirmation dialog that names the object; success gives a toast; failures show the stable error `code` and the `request_id` with a copy button.
5. Forms: visible labels, inline validation next to the field, helper text, disabled-with-reason instead of hidden, unsaved-changes guard.
6. Charts (storage and download trends, scan summaries): line or area with distinct line styles and direct labels, a data table fallback, no more than 6 series.
7. Command palette (Ctrl or Cmd + K): jump to repository, component search, admin pages.
8. Responsive at 375, 768, 1024, 1440 px; no horizontal page scroll; touch targets at least 44 px on touch layouts.
9. Performance: route-level code splitting, virtualized long lists, reserved space for async content (CLS under 0.1), initial JS budget set by measurement in wave 3.
10. i18n-ready strings (English first). No secrets in the DOM: tokens are shown once with an explicit copy action.

## Open items for wave 3

- Light theme tokens and the contrast validator.
- Page list and navigation come from `docs/ui-page-inventory.md` (being produced from the compat and OCI-registry parity research).
- OpenAPI contract for `/api/v1/*` is written after wave 1 integrates, because it needs the final core entities.
