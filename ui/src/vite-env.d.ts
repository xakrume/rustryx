/// <reference types="vite/client" />

// Compile time switches injected by vite.config.ts (and vitest.config.ts). They are
// `define` constants, not import.meta.env values, so the bundler removes the dead
// branch: the production bundle contains neither MSW nor the component demo page.

/** True when the app talks to the in-browser mock (MSW) instead of a backend. */
declare const __API_MOCK__: boolean
/** True when the dev only pages (/__components) are part of the bundle. */
declare const __DEMO__: boolean
/** package.json version of the UI. */
declare const __APP_VERSION__: string

interface ImportMetaEnv {
  readonly VITE_API_BASE_URL?: string
}
