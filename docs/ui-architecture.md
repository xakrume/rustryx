# Web UI architecture (`ui/`)

Status: UI-0 foundation, 2026-10-24. This is the contract between the foundation and every page agent that builds on it. Companion documents: [ui-design-brief.md](ui-design-brief.md) (decisions and rules), [ui-page-inventory.md](ui-page-inventory.md) (pages and states), [api-conventions.md](api-conventions.md) and [openapi/rustryx-api-v1.yaml](openapi/rustryx-api-v1.yaml) (the API contract), `design-system/rustryx/MASTER.md` (tokens).

The UI is a React single page application in `ui/`. It builds to static files in `ui/dist` that the `rustryx` binary embeds and serves next to the API. It talks only to `/api/v1/*`, through a client generated from the OpenAPI document, and it develops against an in-browser mock of that API, so no backend is needed to work on it.

## 1. Stack

| Concern | Choice | Exact version |
|---|---|---|
| UI | React, TypeScript strict (`noUncheckedIndexedAccess`, `verbatimModuleSyntax`) | react 19.3.1, typescript 5.9.3 |
| Build | Vite, `@vitejs/plugin-react` | vite 7.3.7 |
| Styling | Tailwind CSS 4 with CSS variable tokens, class-variance-authority | tailwindcss 4.3.3 |
| Primitives | Radix UI packages one by one, cmdk, lucide-react | see `ui/package.json` |
| Routing | TanStack Router, file based, automatic route level code splitting | 1.170.46 |
| Server state | TanStack Query | 5.104.5 |
| Tables | TanStack Table (headless) | 8.21.3 |
| Charts | Recharts, loaded lazily | 3.10.1 |
| Forms | react-hook-form, zod 4, `@hookform/resolvers` | 7.90.0, 4.6.7, 5.9.2 |
| API client | openapi-typescript (types), openapi-fetch (client) | 7.13.0, 0.17.0 |
| Mock | MSW | 2.15.0 |
| Tests | Vitest, Testing Library, jest-axe, Playwright with `@axe-core/playwright` | vitest 3.2.7, playwright 1.64.1 |
| Fonts | system face first (SF Pro, Segoe UI), Fira Sans and Fira Code through fontsource, self hosted, as fallback | no CDN at runtime; a browser downloads a fallback face only when it uses it |

Versions are pinned exactly in `package.json` and `package-lock.json`. The shadcn/ui components are copied into `src/components/ui` and adapted (see section 10 for why some Radix packages are deliberately not used).

## 2. Commands

Run from `ui/`. Use npm, not yarn or pnpm.

| Command | What it does |
|---|---|
| `npm ci` | install exactly the locked versions |
| `npm run dev` | dev server on 5173; proxies to `RUSTRYX_API_URL` when set, otherwise runs on the mock |
| `npm run mock` | dev server that always uses the mock, even when `RUSTRYX_API_URL` is set |
| `npm run build` | `tsc`, `vite build` to `ui/dist`, then `scripts/check-dist.mjs` (CSP rules, hashed assets, no mock code, size budget) |
| `npm run build:mock` | production style build WITH the mock and the gallery, to `ui/dist-mock` (what the e2e tests run against) |
| `npm run preview` / `preview:mock` | serve a build with the production Content-Security-Policy |
| `npm run typecheck` | `tsc --noEmit` for the app, tests and config |
| `npm run lint` | contrast proof, dash check, ESLint, Prettier check |
| `npm run test` | Vitest (unit and integration, jsdom) |
| `npm run test:coverage` | the same with a v8 coverage report |
| `npm run test:e2e` | Playwright against `build:mock` served by `preview:mock` (chromium) |
| `npm run gen:api` | regenerate `src/api/schema.d.ts` and `src/mocks/generated/operations.json` from the OpenAPI document |
| `npm run license:check` | fail on GPL, LGPL, AGPL, unknown or unlicensed dependencies |
| `npm run check` | typecheck, lint, test and license check in one go |

First time on a machine: `npx playwright install chromium`. Refresh the review screenshots with `SCREENSHOTS=1 npm run test:e2e -- screenshots`.

## 3. Structure

```
ui/
  index.html                 entry; one external blocking script (public/theme-init.js), no inline code
  public/                    theme-init.js (theme before first paint), favicon.svg
  scripts/                   csp.mjs, check-contrast, check-dist, check-licenses, check-dashes, gen-api
  e2e/                       Playwright specs, fixtures, screenshots/
  src/
    main.tsx                 entry, starts the mock only when __API_MOCK__ is true
    app/                     App, router factory, router context, route metadata types
    api/                     generated schema.d.ts, client, errors, events, csrf, query client,
                             pagination, optimistic helpers, service status
    components/ui/           shadcn style primitives (button, dialog, table, dropdown-menu, ...)
    components/              shared components (data-table/, tree/, charts/, form/, confirm-dialog, ...)
    features/<name>/         one folder per feature: api.ts, pages, mocks.ts, tests
    routes/                  TanStack file routes (thin: wiring only)
    hooks/                   generic hooks (debounce, countdown, update-resource, url table state, ...)
    lib/                     utils (cn), format, storage, constants, toast store, zod setup
    i18n/                    en.ts message catalog, typed lookup
    mocks/                   MSW handlers (hand written plus generated from the OpenAPI examples)
    styles/                  index.css, tokens.css (light and dark), fonts.css
    test/                    Vitest setup, jsdom environment, MSW server, render helpers
```

Rules of thumb: a route file only wires (route id, `staticData`, `validateSearch`, the page component); the page and its hooks live in `features/<name>`; anything two features need moves to `components/` or `hooks/`. Files stay under 400 lines.

## 4. Running it

### 4.1 With the mock (no backend)

`npm run dev` without `RUSTRYX_API_URL` starts on the mock. MSW answers `/api/v1/*` in the browser: hand written handlers for sign in, session, the principal, the repository list (filters, sort, keyset paging) and the dashboard, and one handler per OpenAPI operation that has an example (247 of them), generated by `npm run gen:api`. The mock enforces the contract rules that matter to the UI: protected operations answer 401 without a session, a cookie session needs the CSRF header on unsafe methods (403 `csrf_rejected`), single objects carry an ETag, `PUT` and `PATCH` need `If-Match` (428 when missing, 412 when it is the literal `"stale"`).

Test accounts of the mock (fixtures, not credentials of anything real):

| Username | Password | What it is |
|---|---|---|
| `admin` | `mock-password` | local administrator, holds every admin area |
| `alice` | not usable on the form | SSO developer account: browse and upload, no administration |
| `locked` | any | answers 429 `locked_out` with `Retry-After: 30` (lock-out countdown) |
| `unavailable` | any | answers 503 `login_unavailable` |

Other mock switches: `sessionStorage['ryx-mock-sso-down'] = '1'` makes the provider list report SSO as unreachable; `/login?error=oidc_callback_invalid&request_id=abc` shows a failed SSO callback. A service worker cannot mock a top level navigation, so the SSO button goes nowhere in mock mode. The mock session lives in `sessionStorage` of the tab.

### 4.2 With a real backend

```
RUSTRYX_API_URL=http://localhost:8080 npm run dev
```

The dev server proxies `/api` and `/repository` to that address and the mock is off. Because the browser talks to the dev origin, the proxy rewrites the `Origin` header to the backend's own origin; without that every mutation would fail the server's CSRF origin check with 403 `csrf_rejected`. This rewrite exists only in the dev server. The session cookie is `__Host-rustryx_session` (`Secure`, no `Domain`): browsers accept it on `http://localhost`, but a backend on another host needs https. For a self signed development backend set `RUSTRYX_API_INSECURE=1`.

`VITE_API_BASE_URL` points the client at an API on ANOTHER origin. That is not the production setup (one origin) and needs the CORS settings listed in `api-conventions.md` section 11.8 on the server, including `connect-src` widened in the CSP. Leave it empty.

`.env.example` documents both variables. No secret ever goes into the repository or into the bundle.

## 5. API layer

`src/api` is the only place that talks to the network.

**Client.** `api` (from `api/client.ts`) is an `openapi-fetch` client typed by `schema.d.ts`: `api.GET('/api/v1/repositories', { params: { query } })` is checked against the contract. It sends the session cookie (`credentials: 'include'`), adds `X-CSRF-Token` on POST, PUT, PATCH and DELETE (not with a bearer credential), serializes lists comma separated as the contract says, and wraps the platform `fetch`:

- a transport failure becomes an `ApiError` with status 0 and the "server unreachable" banner is told; aborts are cancellations and pass through untouched;
- `403 csrf_rejected` fetches a fresh token from `GET /auth/csrf` and repeats the request once (never for multipart uploads: cloning a streamed body would buffer up to 5 GiB);
- `401` publishes `unauthenticated` (except for the login call itself) and `429`/`503` with `Retry-After` publish the wait, on the event bus (`api/events.ts`).

**Errors.** `unwrap(api.GET(...))` returns the typed body or throws `ApiError` (`code`, `status`, `kind`, `requestId`, `details`, `retryAfterSecs`, `fieldErrors`, `dependents`). A body that is not the envelope (an HTML 502 from a proxy) still yields a usable error with code `http_<status>`. Components never switch on `message`; they switch on `code` or `kind`. `describeError` and `ErrorPanel` show any failure with a title by kind, the server's safe message, and the code and request id with copy buttons.

**Reactions.** `AuthRedirector` turns a 401 into a redirect to `/login?return_to=<where you were>&reason=expired`, dropping everything cached for the old principal. A 403 is shown by `ProtectedRoute` (routes) or `ErrorPanel` (data) as the "You do not have access" panel naming the privilege. `Banners` shows offline, server unreachable, a Retry-After countdown with "Retry now", read-only mode and, for administrators, a degraded system.

**Query defaults** (`api/query-client.ts`): stale-while-revalidate with named stale times (`STALE_TIME.static | principal | default | live`); no retry on 4xx; 429 and 503 retried up to three times honouring `Retry-After`, but only when the wait is at most 60 seconds (a longer wait is a visible countdown the user can act on); network failures retried with backoff; mutations are never retried (a repeated POST can create a second object). Cancellation: pass the `signal` of the query function to the client.

**Pagination.** `useInfinitePage({ queryKey, fetchPage })` implements keyset paging (`{ items, next }`): flat `items`, `loadMore`, no totals. The query key must contain every filter and the sort, because the `after` token is bound to them. A page may hold fewer items than `limit` or none while `next` is not null (permission filtering drops items after the fetch); the hook keeps fetching, at most ten empty pages in a row.

**Concurrency.** `unwrapVersioned` returns `{ data, etag }`. `useUpdateResource` sends the ETag in `If-Match`, updates the cache with the new version, and turns 412 into the three way `ConflictDialog` (reload, overwrite against the freshly read version, copy my changes) and 409 into the same dialog in "state" mode with the blocking objects listed. `optimisticMutation` and `applyOptimistic` give optimistic updates with rollback; updaters must return new objects.

**Session.** `features/auth` holds the session and `/me` queries, login and logout mutations, `permissions.ts` (what the permission summary allows, only to decide what to SHOW), `ProtectedRoute`, `SessionWatcher` (keep-alive refresh while the user is active, a countdown dialog before an idle expiry) and the login page with every error state of the contract. `return_to` is sanitized on the way in and out (no `//host`, no `/\host`, never `/login`).

## 6. Design tokens, themes, density

Tokens live in `src/styles/tokens.css` and nowhere else. The light set is the `@theme` block, the dark set is `:root[data-theme='dark']`; both define exactly the same `--color-*` names (background, foreground, card, popover, primary, secondary, accent, muted, border, input, ring, link, destructive, success, warning, info with `-subtle` and `-text` variants, chart 1 to 6, ...). Spacing (`--space-*`), radius, shadows, fonts, motion and layout sizes are variables too. `--color-*: initial` removes Tailwind's default palette, so `bg-blue-500` does not compile; an ESLint rule rejects raw hex in components.

- `data-theme` on `<html>` is always the resolved `light` or `dark`; "system" is resolved in JavaScript. `public/theme-init.js` sets it (and `data-density`) before first paint from the same localStorage keys the app uses (a test fails if they drift); `ThemeProvider` takes over, follows the OS while the preference is "system", syncs other tabs, and never throws when storage is blocked.
- **Contrast is proven, not claimed.** `scripts/check-contrast.mjs` (part of `npm run lint`) computes the WCAG ratio of every foreground and background pair in both themes (122 pair checks: 4.5:1 for text, 3:1 for control boundaries, the focus ring and graphics; chart series are held to 4.5:1 because they color the direct labels) and fails on any miss. Every token must appear in a checked pair or be exempt with a written reason, so a new token cannot skip the proof. Lowest ratios today: light 3.95, dark 3.29, both for non text graphics. Playwright additionally runs axe in a real browser on the main pages in both themes.
- The accent is Apple blue `#0071e3` in both themes (4.6:1 with white text). Links are `#0066cc` in light and `#2997ff` in dark. Green is the success state only.
- `--color-border` is a decorative separator below 3:1 on purpose (WCAG exempts it); the edge of a control uses `--color-input`.
- Density: `data-density="compact"` (default) or `comfortable` re-points `--control-h-base`, `--row-h-base`, `--cell-px` and the table text size. On coarse pointers `--control-h` and `--row-h` are raised to at least 44px whatever the density says.
- Motion: `prefers-reduced-motion` turns animations into 0.01 ms (Radix waits for `animationend`) and transitions off. Hover changes opacity only, never layout.
- Fonts: the stack starts with the system face (`-apple-system`, SF Pro, Segoe UI) and the system monospace; the self-hosted fallback is Fira Sans 400 to 700 (latin, latin-ext, cyrillic) and Fira Code Variable (same subsets) as woff2 from `@fontsource`, declared in `fonts.css` with the vendor's `unicode-range`, so the browser downloads only the subsets a page needs. Fira Code is for technical values only (coordinates, digests, paths, commands, logs).

To add a token: add it to BOTH blocks, then run `npm run lint:contrast`; add a pair to `PAIRS` in `scripts/contrast-lib.mjs` (or an exemption with a reason).

## 7. Shell

`routes/_app.tsx` is the pathless layout of every page in the shell; `routes/login.tsx` and `routes/__root.tsx` sit outside it.

- **Layout** (`features/shell/app-shell.tsx`): sidebar from 768px (an icon rail by default under 1024px, collapse state remembered), a drawer below, a top bar, banners, and ONE scroll area (`<main id="main-content" tabIndex={-1}>`), so the page behind a dialog has nothing of its own to scroll.
- **Navigation** (`nav-config.ts`): the tree of the page inventory, M1 entries. Each item has a `need` (a permission requirement); items the user cannot use are hidden, not disabled. Administration groups start collapsed unless the current page is inside. Up, Down, Home and End move between items, every item is still a Tab stop. A page agent edits this file when its page leaves the placeholder state.
- **Route metadata** (`app/route-meta.d.ts`): `staticData.crumb` (a message key, or `({ params }) => string` for dynamic names) feeds the breadcrumbs, shown from two crumbs up; `staticData.access` (`'public'`, `'authenticated'`, `'admin'`, `{ area }`, `{ can }`, `{ repository, action }`, or a function) is enforced by `ProtectedRoute` for every matched route, so a layout route can protect a subtree. Anonymous visitors who fail it go to the login page and come back; signed in users get the forbidden panel.
- **Focus and announcements** (`route-focus-manager.tsx`): after a route change that alters the pathname, focus moves to the page `<h1>` (`data-page-title`, rendered by `PageHeader`; if the page body is still loading, to `<main>` first and to the heading when it appears) and the new title is announced in a polite live region. A change of the query string (typing in a filter) never steals focus. The skip link is the first focusable element of every page.
- **Command palette** (Ctrl or Cmd plus K, from anywhere): pages (filtered by permissions), repositories matching the typed text, a component search, theme and density and sign out. A feature adds commands with `useCommandPalette().register([...])` in an effect and removes them on unmount.
- **Errors**: unknown address renders `NotFoundPage`; a thrown render error is caught by the router boundary (`RouteErrorPage`: API failures with code and request id and a Retry, anything else as "This page crashed" without leaking the text) and, outside the router, by `ErrorBoundary`.

## 8. Shared components

All in `src/components`, each with a Testing Library test, a jest-axe check and keyboard tests where relevant. `/__components` (development and mock builds only, removed from the production bundle) shows them all and is the page to open for visual checks in both themes, both densities and at 375, 768 and 1440 px.

| Component | Notes |
|---|---|
| `DataTable`, `FilterBar` (`data-table/`) | server side sort (`aria-sort`, API values `name` and `-name`), debounced filters with chips, keyset "load more" and page size, column chooser remembered per `tableId`, sticky header, row selection with a bulk action bar (cleared when filters change), loading skeleton after 150 ms, first use and filtered empty states, error panel with retry, a keyboard focusable horizontal scroll region, one line cells |
| `useUrlTableState` | filters and sort in the address bar (`?q=&format=&sort=-name`); the route must validate its search with strings |
| `ConfirmDialog` | names the object in the title, lists dependents, optional typing of the exact name, Cancel focused first, errors stay in the dialog |
| `ConflictDialog` + `useUpdateResource` | 412 and 409 handling described above |
| `FormField`, `InlineField`, `TextField`, `TextareaField`, `SelectField`, `CheckboxField`, `SwitchField`, `Form`, `useZodForm`, `applyApiFieldErrors`, `UnsavedChangesGuard` | visible label, required marker for assistive tech, helper text, inline error with icon and text, ids and `aria-describedby` wired, first invalid field focused, server `details.fields` JSON pointers mapped onto fields |
| `CopyButton`, `CodeBlock` | Fira Code, copy with toast and a fallback for non-secure contexts; a secret block keeps the value OUT of the DOM until revealed |
| `SecretOnce` | show-once dialog; cannot be closed by Escape, outside click or the close button before "I stored it" is ticked |
| `StatusBadge` | icon plus text, never color alone |
| `BytesText`, `RelativeTime` | binary units with the exact count as tooltip; relative text, machine `datetime`, absolute time in a tooltip and visually hidden for screen readers |
| `Tree` | ARIA tree: roving tab stop, arrows, Home, End, `*`, type-ahead, lazy children with busy state and retry, no interactive element inside rows |
| `TimeSeriesChart` (`charts/chart.tsx`, lazy) | line or area, at most six series, distinct dash patterns and marker shapes, direct labels pushed apart, a data table behind it (also in the DOM for screen readers); colors are resolved from the tokens (`useCssColors`) because SVG attributes cannot use `var()` |
| `EmptyState`, `PageHeader`, `ErrorPanel`, `ForbiddenPanel`, `PrivilegeGate` | `PrivilegeGate` hides by default, or hands `allowed` to a render function for "disabled with a reason" |
| `Toaster`, `toast.*` | Radix Toast; `toast.apiError(error, context)` shows code and request id with copy buttons |
| `ui/*` | button, input, select (native), checkbox, switch, badge, card, skeleton, separator, table, tabs, tooltip, popover, dropdown-menu (non modal), dialog and sheet (custom, see 10), command, alert, kbd |

## 9. Adding a feature (what a page agent does)

Example: the repository list page.

1. **Route.** The route file already exists (every M1 page of the inventory is a placeholder in `src/routes/`, so the route tree is final and agents do not conflict). Open it, set `staticData` (`crumb`, `access`), add `validateSearch` for list state (hand written, no zod: see 10), and replace the placeholder component with your page:
   ```tsx
   export const Route = createFileRoute('/_app/repositories/')({
     staticData: { access: 'public' },
     validateSearch: (s: Record<string, unknown>) => ({ q: text(s.q), sort: text(s.sort) }),
     component: RepositoriesPage,
   })
   ```
2. **API hooks** in `src/features/repositories/api.ts`: a key factory and `queryOptions` built from the typed client (see `features/dashboard/api.ts` for the shape). Lists use `useInfinitePage`; reads of editable objects use `unwrapVersioned` so the ETag travels with the data; writes use `useMutation` (or `useUpdateResource` for versioned objects) and invalidate the keys they affect.
3. **Page** in `src/features/repositories/*.tsx`: `PageHeader` first, `DataTable` for lists, `ErrorPanel` for failures, `PrivilegeGate` for actions, `ConfirmDialog` for destructive ones, strings from `src/i18n/en.ts` (add a namespace).
4. **Navigation.** Entry in `nav-config.ts` if it is not there yet; breadcrumbs through `crumb`.
5. **Mock.** Anything with a generated example already works. For stateful or computed answers add `src/features/repositories/mocks.ts` exporting `handlers: HttpHandler[]`; it is discovered by `import.meta.glob`, no shared file to edit. Use the helpers of `src/mocks/store.ts` (`json`, `visibleRepositories`, `LATENCY_MS`) and `src/mocks/session.ts`.
6. **Tests.** Component tests with `renderWithProviders`, whole page tests with `renderApp('/repositories', { user: 'admin', overrides: [...] })` against the same mock, jest-axe through `expectNoA11yViolations`, a Playwright spec in `e2e/` for the main flow. Every negative requirement gets a test whose name starts with `must_not_`.
7. **Check.** `npm run check`, then `npm run build` and `npm run test:e2e`.

Conventions every feature follows:

- Strings through `t()`; no raw hex or `style=` in markup; no `console.log`; comments explain why and name the mechanism.
- `ApiError.code` for logic, never `message`. Show `code` and `request_id` on every failure.
- Immutable updates (spread, never mutate cache or state objects).
- Import `@/lib/zod-setup` (side effect) before building the first zod schema in a module.
- Do not use `<Navigate>` for a redirect that depends on a new object every render (it loops); use `router.navigate` once, as `ProtectedRoute` does.
- Do not run `shadcn add` blindly: its dialog, select, scroll-area and sonner break the CSP (section 10). Copy and adapt.

## 10. Content-Security-Policy and delivery

### 10.1 The policy the build is designed for

One source: `ui/scripts/csp.mjs`. `vite preview` (and so the e2e tests) send exactly this string, `check-dist.mjs` verifies the build against it, and the Rust server sends the same value with the static files:

```
default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self'; font-src 'self';
connect-src 'self'; worker-src 'self'; manifest-src 'self'; object-src 'none'; base-uri 'none';
form-action 'self'; frame-ancestors 'none'
```

plus `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, `X-Frame-Options: DENY`, and HSTS from the TLS terminator. No `'unsafe-inline'`, no `'unsafe-eval'`, no `data:` and no `blob:` source.

What that means in practice, and how it is held:

- **No inline script.** The theme bootstrap is the external blocking file `/theme-init.js`, which runs before first paint (a module script would flash light for dark users). `index.html` is checked for inline `<script>`, `<style>`, `style=` and `on*=` by `check-dist.mjs` and a unit test.
- **No inline style element or attribute in markup.** React sets `style` props through the CSSOM, which CSP does not block. Libraries that append a `<style>` element at runtime ARE blocked, so they are not used: `@radix-ui/react-dialog`, `-alert-dialog`, `-select` and modal menus (react-remove-scroll), `-scroll-area`, and `sonner`. Replacements: a custom `Dialog` built from the Radix focus scope, dismissable layer, portal and presence primitives (scroll lock is the CSS rule `body:has([data-dialog-open])`); `DropdownMenu` rendered non modal; a styled native `<select>`; Radix Toast. Tests assert that opening a dialog adds no `<style>` element.
- **No eval.** zod 4 probes `new Function('')`; under this policy that is reported as a violation, so `lib/zod-setup.ts` switches the JIT off (`z.config({ jitless: true })`).
- **Fonts and images from `'self'`.** `assetsInlineLimit` is 0, so nothing becomes a `data:` URI; fonts are woff2 files in `dist/assets`.
- **`connect-src 'self'`.** The API is same origin. An API on another origin means the operator widens this directive on purpose.
- The Playwright fixtures fail any test on a `securitypolicyviolation` event, an uncaught page error or a console error, so a regression is caught in CI, not in a browser of a customer.

### 10.2 How the Rust server embeds and serves `ui/dist`

The build output is plain static files: `index.html`, `theme-init.js`, `favicon.svg` and `assets/` (content hashed: `index-<hash>.js`, `<name>-<hash>.css`, `<font>-<hash>.woff2`). The server embeds the directory at compile time (for example with `rust-embed` or `include_dir`; `ui/dist` is produced by `npm ci && npm run build` before `cargo build`, in the release pipeline and the Dockerfile) and serves it from the root of the UI listener with these rules:

| Request | Response |
|---|---|
| `GET`/`HEAD` of a file that exists in the embedded set | that file, with its content type |
| a path under `/assets/` that does not exist | `404`, NOT `index.html` (a stale hashed name must fail loudly, not return HTML as JavaScript) |
| any other `GET` that accepts `text/html` and is not an API or protocol prefix | `index.html` with status `200` (SPA fallback); the router decides what the path means |
| `/api/`, `/repository/`, `/v2/`, `/service/`, `/healthz`, `/readyz`, `/metrics` | never the fallback: they belong to the API, the format protocols and the operations endpoints |
| other methods | `405` |

Headers on every UI response: the policy and the three headers above. Caching: `assets/*` get `Cache-Control: public, max-age=31536000, immutable` (the hash is the version); `index.html`, `theme-init.js` and `favicon.svg` get `Cache-Control: no-cache` (revalidate by ETag), because they are not hashed and `index.html` names the current assets. Compress text responses (gzip, brotli) as for the API; woff2 is already compressed. The UI needs no server side templating: there is no per request nonce, no base URL injection (the UI uses the origin it is served from), and no environment specific file.

The embedded set is versioned with the binary, so `GET /api/v1/system/info` (version, build) always describes both halves. A client of an older browser tab after an upgrade may request an old hashed asset: it gets 404, the app fails to load that chunk, and a reload fixes it (the router error page offers it).

## 11. Testing

- **Unit and integration** (`npm run test`, 38 files, 269 tests, jsdom): helpers and hooks, the API layer against MSW (CSRF repair, Retry-After, abort, pagination, ETag and conflicts), every shared component with jest-axe, the login flow, route protection, the shell (focus, palette, banners, theme) through `renderApp`. jest-axe runs with `color-contrast` and `region` off, because jsdom has neither layout nor a whole page; contrast is covered by the token script and the Playwright axe scan. Coverage today (`npm run test:coverage`, routes, demo and entry points excluded): 93.1% statements, 89.9% branches, 88.1% functions.
- **Test environment**: `src/test/jsdom-environment.ts` keeps Node's own `AbortController` (jsdom's is rejected by Node's `fetch`, which would make every API call in a test fail), and `setup.ts` installs an in memory `Storage` (newer Node versions ship a `localStorage` global that shadows jsdom's).
- **End to end** (`npm run test:e2e`, 51 tests, chromium): smoke test of the shell (mock sign in, navigation, theme, command palette, keyboard only sidebar navigation, 375 px drawer with 44 px targets, no horizontal scroll at 375, 768 and 1440 in both themes), the component gallery against the mock API (server sort, filters, paging, selection, dialogs, tree, chart, form), and axe scans of the main pages and overlays in both themes. They run against the production style build with the production CSP.
- **Naming**: tests of a negative requirement start with `must_not_` (CONTRIBUTING rule 8).

## 12. Accessibility checklist per page

A page is done when all of these hold (the shell provides the first four):

1. One `<h1>` through `PageHeader`; it gets focus after navigation; `document.title` follows it.
2. Skip link works; the page content is inside the shell's `<main>`; landmarks are named.
3. Everything operable by keyboard in a logical order; a visible focus ring everywhere (the global `:focus-visible` rule); no keyboard trap; dialogs trap and restore focus.
4. Text 4.5:1 and non text 3:1 in BOTH themes by using tokens only; checked by the Playwright axe scan: add the page to `e2e/a11y.spec.ts`.
5. Status and validation never by color alone: icon plus text (`StatusBadge`, inline errors).
6. Every control has a visible label (placeholders are not labels); required fields are marked for assistive tech too; errors are tied to fields (`aria-describedby`, `aria-invalid`) and the first invalid field is focused.
7. Icon buttons have an accessible name; decorative icons are `aria-hidden`.
8. Tables have a name, header cells with `scope`, `aria-sort` on sortable columns; wide ones scroll in a focusable region.
9. Loading states reserve space (no layout jump), announce with `aria-busy` or a status; empty, filtered, error, 403, 404 and 409 states are designed (inventory section 5).
10. Works at 375, 768, 1024 and 1440 px without horizontal page scroll; touch targets at least 44 px on touch layouts (use `--control-h`).
11. Motion respects `prefers-reduced-motion` (the global rule); no information only in a tooltip.
12. Destructive actions use `ConfirmDialog` naming the object (typed name when irreversible); success gives a toast; failure shows code and request id.
13. Tokens and secrets are shown once (`SecretOnce`), never stored in the browser or the DOM.

## 13. Bundle size

Measured on the production build of 2026-11-06 (`npm run build`, `check-dist.mjs`):

| | raw | gzip |
|---|---|---|
| initial JavaScript (entry, `vendor-react`, `vendor-tanstack`) | 483.5 KiB | **155.8 KiB** |
| of which application entry | 154.6 KiB | 50.9 KiB |
| of which React and React DOM | 217.1 KiB | 67.5 KiB |
| of which TanStack Router and Query | 111.9 KiB | 37.4 KiB |
| initial CSS | | 12.4 KiB |

The signed-in shell (`_app`) carries tooltips and toasts so the login entry stays light; Recharts and the table library load only on pages that need them. The build fails when the initial JavaScript passes 175 KiB gzip (`INITIAL_JS_GZIP_BUDGET_KIB` in `scripts/check-dist.mjs`). `cn()` uses `clsx` only (no `tailwind-merge`) so merge logic does not land in the entry. Zod stays out of the entry (hand-written `validateSearch`).

## 14. Generated files and CI

`src/api/schema.d.ts` and `src/mocks/generated/operations.json` are generated from `docs/openapi/rustryx-api-v1.yaml` and committed. CI should fail when they are stale:

```
cd ui && npm ci && npm run gen:api && git diff --exit-code src/api src/mocks/generated
```

Suggested CI steps for the `ui` job: `npm ci`, `npm run check` (typecheck, lint with the contrast proof, tests, licenses), `npm run build`, `npx playwright install --with-deps chromium`, `npm run test:e2e`; keep `ui/playwright-report` as an artifact on failure. `npm run license:check` applies CONTRIBUTING's policy (MIT compatible; MPL-2.0, CC-BY data and the OFL fonts are explicitly allowed; GPL, LGPL and AGPL fail). `npm run lint:text` enforces the no em dash or en dash rule over `ui/` and this document.

## 15. Intentionally not done in UI-0

- **No real pages.** Every M1 route exists as a placeholder with its final path, breadcrumb and access rule; the pages themselves (repositories, browse, search, upload, administration, account) are for the page agents. The dashboard is the one worked example of a feature module. M2 and M3 pages and their navigation entries are not created.
- **Reusable components from the inventory that no UI-0 page needs yet**: the `client setup` panel, attribute and checksum panel, severity bar, rule list editor with a live tester, dry-run report viewer, schedule picker, scope picker, audit event row, job toast and run page. They are built with the first page that needs them.
- **No virtualization** of long tables or trees (rule 9 of the brief): lists grow only as the user loads more; the Tree and the table markup are flat and virtualizable later.
- **No second language.** Strings are in `src/i18n/en.ts` and looked up through `t()`; a Russian catalog is a second file with the same shape.
- **No SSO in the mock.** A service worker cannot mock the redirect; the button, its states and the callback error page are covered, the flow is not.
- **No visual regression suite.** Screenshots in `ui/e2e/screenshots` are for human review.
- **No i18n of dates beyond `Intl`** with the browser locale, and no per user time zone setting (absolute times are local, with the UTC instant in the tooltip).
- **No service worker, PWA or offline mode** beyond the banner and stale-while-revalidate.
- **No per area privileges.** Navigation and routes follow the phase 1 rule (`nx-admin` implies every area); `hasArea` already reads `admin_areas` for AUTH-46.
- **The e2e suite is chromium only.**

## 16. Decisions and deviations from the brief

| Topic | Decision | Reason |
|---|---|---|
| Accent | Apple blue `#0071e3` with white text in both themes | 4.6:1 with white text; proven by the contrast check |
| Dialog, menu, select, toast | custom Dialog on Radix primitives, non modal menus, native select, Radix Toast | the stock components inject `<style>` at runtime, which `style-src 'self'` blocks |
| TypeScript 5.9, ESLint 9, Vitest 3, Table 8 | not the newest majors | typescript-eslint and eslint-plugin-jsx-a11y do not support TypeScript 6 and ESLint 10 yet; TanStack Table 9 has a different API |
| `validateSearch` hand written | no zod in route files | the router needs it on the critical path; zod would be in the entry bundle |
| Single `tsconfig.json` | app, tests and config in one program | one type check, type aware linting for everything |
| Toasts for every copy | `CopyButton` toasts by default | the brief asks for copy with a toast; `silent` turns it off where the context announces it |
