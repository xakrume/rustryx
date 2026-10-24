import type { components } from '@/api/schema'

type FormatInfo = components['schemas']['FormatInfo']
type UploadSchema = components['schemas']['UploadSchema']
type UploadField = components['schemas']['UploadField']

const ALL = [
  'older_than_days',
  'last_downloaded_days',
  'keep_last_versions',
  'name_pattern',
] as const

const info = (
  format: FormatInfo['format'],
  label: string,
  overrides: Partial<FormatInfo>,
): FormatInfo => ({
  format,
  label,
  supports: { hosted: true, proxy: true, group: true },
  default_write_policy: 'allow',
  uploadable_in_ui: true,
  cleanup_criteria: [...ALL],
  config_schema: { type: 'object', properties: {} },
  client_ids: [],
  ...overrides,
})

/** The formats of the mock node, with the `config_schema` each one would publish. */
export const MOCK_FORMATS: readonly FormatInfo[] = [
  info('maven2', 'Maven 2', {
    default_write_policy: 'allow_once',
    cleanup_criteria: [...ALL, 'release_type'],
    config_schema: {
      type: 'object',
      properties: {
        version_policy: {
          title: 'Version policy',
          description: 'Which kinds of versions the repository accepts.',
          enum: ['release', 'snapshot', 'mixed'],
          default: 'release',
        },
        layout_policy: {
          title: 'Layout policy',
          description: 'Strict rejects paths that do not follow the Maven layout.',
          enum: ['strict', 'permissive'],
          default: 'strict',
        },
      },
    },
    client_ids: ['mvn', 'gradle'],
  }),
  info('npm', 'npm', {
    config_schema: {
      type: 'object',
      properties: {
        strict_content_type: {
          title: 'Strict content type',
          description: 'Refuse files whose content type does not match npm packages.',
          type: 'boolean',
          default: true,
        },
      },
    },
    client_ids: ['npm'],
  }),
  info('pypi', 'PyPI', {
    config_schema: {
      type: 'object',
      properties: {
        strict_content_type: { title: 'Strict content type', type: 'boolean', default: true },
        max_versions_listed: {
          title: 'Versions listed per package',
          description: 'Upper bound of versions shown in the simple index. 0 means all.',
          type: 'integer',
          default: 0,
        },
      },
    },
    client_ids: ['pip', 'twine'],
  }),
  info('docker', 'Docker', {
    default_write_policy: 'allow',
    uploadable_in_ui: false,
    cleanup_criteria: ['older_than_days', 'last_downloaded_days', 'name_pattern'],
    config_schema: {
      type: 'object',
      properties: {
        allow_anonymous_pull_of_blobs: {
          title: 'Allow anonymous blob pull',
          description: 'Let anonymous clients fetch layers when the manifest is public.',
          type: 'boolean',
          default: false,
        },
      },
    },
    client_ids: ['docker'],
  }),
  info('oci', 'OCI', {
    uploadable_in_ui: false,
    cleanup_criteria: ['older_than_days', 'last_downloaded_days', 'name_pattern'],
    client_ids: ['docker', 'helm'],
  }),
  info('helm', 'Helm', {
    cleanup_criteria: ['older_than_days', 'last_downloaded_days', 'keep_last_versions'],
    config_schema: {
      type: 'object',
      properties: {
        index_cache_secs: {
          title: 'Index cache (seconds)',
          description: 'How long the generated index.yaml is reused.',
          type: 'integer',
          default: 60,
        },
      },
    },
    client_ids: ['helm'],
  }),
  info('raw', 'Raw', {
    supports: { hosted: true, proxy: true, group: true },
    cleanup_criteria: ['older_than_days', 'name_pattern'],
    config_schema: {
      type: 'object',
      properties: {
        content_disposition: {
          title: 'Content disposition',
          enum: ['attachment', 'inline'],
          default: 'attachment',
        },
      },
    },
    client_ids: ['curl'],
  }),
  info('pub', 'Dart pub', {
    default_write_policy: 'allow_once',
    cleanup_criteria: ['older_than_days', 'last_downloaded_days', 'keep_last_versions'],
    client_ids: ['dart'],
  }),
]

const field = (f: Partial<UploadField> & Pick<UploadField, 'name' | 'label'>): UploadField => ({
  type: 'string',
  required: false,
  scope: 'component',
  description: null,
  pattern: null,
  enum_values: null,
  derive_from: null,
  ...f,
})

const base = {
  uploadable: true,
  reason: null,
  multi_file: false,
  min_files: 1,
  max_files: 1,
  accepted_extensions: [] as string[],
  max_file_bytes: 5_368_709_120,
} as const

/** The upload forms of the formats. Docker and OCI are published with their own protocol. */
export function mockUploadSchema(format: FormatInfo['format']): UploadSchema | null {
  switch (format) {
    case 'maven2':
      return {
        ...base,
        format,
        multi_file: true,
        max_files: 20,
        accepted_extensions: ['.jar', '.pom', '.war', '.zip'],
        fields: [
          field({
            name: 'group_id',
            label: 'Group ID',
            required: true,
            pattern: '^[A-Za-z0-9_.-]+$',
            derive_from: 'pom',
          }),
          field({ name: 'artifact_id', label: 'Artifact ID', required: true, derive_from: 'pom' }),
          field({ name: 'version', label: 'Version', required: true, derive_from: 'pom' }),
          field({
            name: 'packaging',
            label: 'Packaging',
            type: 'enum',
            enum_values: ['jar', 'war', 'pom', 'zip'],
            default: 'jar',
          }),
          field({
            name: 'extension',
            label: 'Extension',
            required: true,
            scope: 'asset',
            derive_from: 'filename',
          }),
          field({ name: 'classifier', label: 'Classifier', scope: 'asset' }),
        ],
      }
    case 'npm':
      return {
        ...base,
        format,
        accepted_extensions: ['.tgz'],
        fields: [
          field({
            name: 'tag',
            label: 'Dist tag',
            default: 'latest',
            description: 'The package.json inside the file names the package and version.',
          }),
        ],
      }
    case 'pypi':
      return {
        ...base,
        format,
        multi_file: true,
        max_files: 10,
        accepted_extensions: ['.whl', '.tar.gz'],
        fields: [
          field({
            name: 'requires_python',
            label: 'Requires Python',
            derive_from: 'package_metadata',
          }),
        ],
      }
    case 'helm':
      return { ...base, format, accepted_extensions: ['.tgz'], fields: [] }
    case 'pub':
      return { ...base, format, accepted_extensions: ['.tar.gz'], fields: [] }
    case 'raw':
      return {
        ...base,
        format,
        multi_file: true,
        max_files: 50,
        fields: [
          field({
            name: 'directory',
            label: 'Directory',
            required: true,
            pattern: '^[A-Za-z0-9_./-]+$',
            description: 'Folder inside the repository, for example releases/2026.',
          }),
          field({ name: 'filename', label: 'File name in the repository', scope: 'asset' }),
        ],
      }
    default:
      return {
        ...base,
        format,
        uploadable: false,
        reason:
          'Container images and OCI artifacts are published with their own client protocol, not through the browser.',
        min_files: 0,
        max_files: 0,
        fields: [],
      }
  }
}
