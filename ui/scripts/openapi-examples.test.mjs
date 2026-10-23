// @vitest-environment node
import { describe, expect, it } from 'vitest'
import { extractOperations, resolveRef } from './openapi-examples.mjs'

const spec = {
  security: [{ sessionCookie: [] }],
  paths: {
    '/api/v1/things': {
      get: {
        operationId: 'listThings',
        security: [{}, { sessionCookie: [] }],
        responses: {
          200: {
            content: { 'application/json': { schema: { $ref: '#/components/schemas/ThingPage' } } },
          },
          500: { $ref: '#/components/responses/Boom' },
        },
      },
      post: {
        operationId: 'createThing',
        responses: {
          201: {
            content: {
              'application/json': {
                examples: { first: { value: { id: 'new' } }, second: { value: { id: 'other' } } },
              },
            },
          },
        },
      },
    },
    '/api/v1/things/{id}': {
      delete: { operationId: 'deleteThing', responses: { 204: { description: 'gone' } } },
      get: {
        operationId: 'getThing',
        responses: {
          200: {
            content: {
              'application/json': { schema: { allOf: [{ $ref: '#/components/schemas/Thing' }] } },
            },
          },
        },
      },
      put: {
        operationId: 'noExample',
        responses: { 200: { content: { 'application/json': { schema: { type: 'object' } } } } },
      },
    },
  },
  components: {
    schemas: {
      ThingPage: { example: { items: [{ id: 1 }], next: null } },
      Thing: { example: { id: 1 } },
    },
    responses: { Boom: { description: 'x' } },
  },
}

describe('extractOperations', () => {
  const ops = extractOperations(spec)
  const byId = Object.fromEntries(ops.map((o) => [o.operationId, o]))

  it('takes the schema example of a referenced response schema', () => {
    expect(byId.listThings?.example).toEqual({ items: [{ id: 1 }], next: null })
    expect(byId.listThings?.status).toBe(200)
  })

  it('takes the first of several response examples', () => {
    expect(byId.createThing).toMatchObject({ status: 201, example: { id: 'new' } })
  })

  it('follows allOf to find an example', () => {
    expect(byId.getThing?.example).toEqual({ id: 1 })
  })

  it('keeps bodiless 204 answers and drops JSON answers without any example', () => {
    expect(byId.deleteThing).toMatchObject({ status: 204 })
    expect(byId.deleteThing).not.toHaveProperty('example')
    expect(byId.noExample).toBeUndefined()
  })

  it('records whether the contract allows anonymous callers', () => {
    expect(byId.listThings?.anonymous).toBe(true)
    expect(byId.createThing?.anonymous).toBe(false)
  })

  it('must_not_follow_a_reference_outside_the_document', () => {
    expect(() => resolveRef(spec, 'https://evil.example/schema.json')).toThrow()
    expect(() => resolveRef(spec, '#/components/schemas/Missing')).toThrow()
  })
})
