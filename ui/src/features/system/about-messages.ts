/** Strings of the About page. */
export const aboutPageMessages = {
  title: 'About Rustryx',
  description: 'Version, build and supply chain information of this server.',
  loadFailed: 'The server information could not be loaded',
  product: {
    title: 'Product',
    name: 'Name',
    version: 'Version',
    license: 'License',
  },
  build: {
    title: 'Build',
    commit: 'Commit',
    builtAt: 'Built',
    profile: 'Build profile',
    unknown: 'Not reported',
  },
  supply: {
    title: 'Supply chain',
    sbom: 'Software bill of materials',
    sbomLink: 'Open the SBOM',
    sbomNone: 'This server does not publish an SBOM.',
    signature: 'Verify the signature',
    signatureNone: 'No signature verification hint is published.',
    licenses: 'Third-party licenses',
    licensesLink: 'Read the third-party licenses',
    licensesNone: 'No license list is published.',
    newTab: '(opens in a new tab)',
  },
  banner: 'Message from the administrators',
} as const
