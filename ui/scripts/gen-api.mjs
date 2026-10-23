// Regenerates everything that is derived from the OpenAPI contract:
//   1. src/api/schema.d.ts            TypeScript types for openapi-fetch (openapi-typescript)
//   2. src/mocks/generated/operations.json   one response example per operation, for the mock
//
// Source of truth: docs/openapi/rustryx-api-v1.yaml (never edit the outputs by hand).
// CI check that the committed outputs are current:
//   npm run gen:api && git diff --exit-code src/api src/mocks/generated
import { execFileSync } from 'node:child_process'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { parse } from 'yaml'
import { extractOperations } from './openapi-examples.mjs'

const uiRoot = fileURLToPath(new URL('..', import.meta.url))
const specPath = fileURLToPath(new URL('../../docs/openapi/rustryx-api-v1.yaml', import.meta.url))
const schemaOut = fileURLToPath(new URL('../src/api/schema.d.ts', import.meta.url))
const opsOut = fileURLToPath(new URL('../src/mocks/generated/operations.json', import.meta.url))

execFileSync(
  fileURLToPath(new URL('../node_modules/.bin/openapi-typescript', import.meta.url)),
  [specPath, '-o', schemaOut],
  { cwd: uiRoot, stdio: 'inherit' },
)

const spec = parse(readFileSync(specPath, 'utf8'))
const operations = extractOperations(spec)
mkdirSync(fileURLToPath(new URL('../src/mocks/generated', import.meta.url)), { recursive: true })
writeFileSync(
  opsOut,
  JSON.stringify({ specVersion: spec.info.version, operations }, null, 1) + '\n',
)
console.log(
  `gen:api: ${operations.length} operations with a response example (spec ${spec.info.version})`,
)
