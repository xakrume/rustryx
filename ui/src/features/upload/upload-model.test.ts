import { describe, expect, it } from 'vitest'
import { mockUploadSchema } from '@/features/repositories/mock-formats'
import {
  buildAttributes,
  fieldsOf,
  fileProgress,
  hasErrors,
  initialFields,
  mapServerErrors,
  noErrors,
  validate,
  type PickedFile,
  type UploadFormState,
} from './upload-model'

const maven = mockUploadSchema('maven2')!
const file = (name: string, size = 10): PickedFile => ({
  id: name,
  file: new File([new Uint8Array(size)], name),
})

function state(overrides: Partial<UploadFormState> = {}): UploadFormState {
  return {
    fields: initialFields(maven),
    files: [file('lib-1.0.jar')],
    assetFields: {},
    ...overrides,
  }
}

describe('upload form model', () => {
  it('starts from the schema defaults', () => {
    expect(initialFields(maven)).toEqual({
      group_id: '',
      artifact_id: '',
      version: '',
      packaging: 'jar',
    })
    expect(fieldsOf(maven, 'asset').map((f) => f.name)).toEqual(['extension', 'classifier'])
  })

  it('accepts an empty form for a format that can read its fields from the file', () => {
    // group, artifact and version are required but derivable from the POM: not required of the user.
    expect(hasErrors(validate(maven, state()))).toBe(false)
  })

  it('demands a required field the server cannot derive, and checks patterns and integers', () => {
    const schema = {
      ...maven,
      fields: [
        {
          name: 'directory',
          label: 'Directory',
          type: 'string',
          required: true,
          scope: 'component',
          pattern: '^[a-z/]+$',
        },
        { name: 'build', label: 'Build', type: 'integer', required: false, scope: 'component' },
      ],
    } as typeof maven
    const empty = validate(schema, {
      fields: { directory: '', build: '' },
      files: [file('a.txt')],
      assetFields: {},
    })
    expect(empty.fields.directory).toBe('Directory is required.')
    const bad = validate(schema, {
      fields: { directory: 'UPPER', build: '1.5' },
      files: [file('a.txt')],
      assetFields: {},
    })
    expect(bad.fields.directory).toBe('Directory is not in the expected format.')
    expect(bad.fields.build).toBe('Build must be a whole number.')
    expect(hasErrors(bad)).toBe(true)
  })

  it('checks extensions, size and the number of files', () => {
    const picked = validate(maven, state({ files: [file('notes.txt')] }))
    expect(picked.file['notes.txt']).toMatch(/not accepted/)
    const huge = validate({ ...maven, max_file_bytes: 5 }, state({ files: [file('a.jar', 10)] }))
    expect(huge.file['a.jar']).toMatch(/larger than/)
    expect(validate(maven, state({ files: [] })).files).toBe('Choose at least one file.')
    const many = Array.from({ length: 21 }, (_, i) => file(`f${i}.jar`))
    expect(validate(maven, state({ files: many })).files).toBe('Choose at most 20 files.')
  })

  it('builds the attributes part: component fields once, asset fields per file in order', () => {
    const files = [file('a.jar'), file('a-sources.jar')]
    const attributes = buildAttributes(
      maven,
      state({
        fields: { group_id: 'com.example', artifact_id: ' ', version: '1.0', packaging: 'jar' },
        files,
        assetFields: { 'a-sources.jar': { extension: 'jar', classifier: 'sources' } },
      }),
    )
    expect(attributes.fields).toEqual({ group_id: 'com.example', version: '1.0', packaging: 'jar' })
    expect(attributes.assets).toEqual([
      { fields: {}, sha256: null },
      { fields: { extension: 'jar', classifier: 'sources' }, sha256: null },
    ])
  })

  it('sends booleans and integers with their own types', () => {
    const schema = {
      ...maven,
      fields: [
        { name: 'flag', label: 'Flag', type: 'boolean', required: false, scope: 'component' },
        { name: 'n', label: 'N', type: 'integer', required: false, scope: 'component' },
      ],
    } as typeof maven
    expect(
      buildAttributes(schema, { fields: { flag: true, n: '7' }, files: [], assetFields: {} })
        .fields,
    ).toEqual({ flag: true, n: 7 })
  })

  it('says how far each file is from the bytes sent so far', () => {
    expect(fileProgress([100, 100], 0, 200, 0)).toEqual([0, 0])
    expect(fileProgress([100, 100], 150, 200, 150)).toEqual([1, 0.5])
    expect(fileProgress([100, 100], 200, 200, 200)).toEqual([1, 1])
    expect(fileProgress([100], 500, 400, 400)).toEqual([1])
    expect(fileProgress([0, 10], 5, 15, 15)).toEqual([1, 1])
  })

  it('maps field errors of the server onto component fields, files and the form', () => {
    const files = [file('a.jar'), file('b.jar')]
    const errors = mapServerErrors(
      [
        { field: '/fields/group_id', message: 'bad group' },
        { field: '/assets/1/fields/extension', message: 'bad extension' },
        { field: '/other', message: 'something else' },
      ],
      files,
    )
    expect(errors.fields).toEqual({ group_id: 'bad group' })
    expect(errors.assetFields).toEqual({ 'b.jar': { extension: 'bad extension' } })
    expect(errors.files).toBe('something else')
    expect(hasErrors(noErrors)).toBe(false)
  })
})
