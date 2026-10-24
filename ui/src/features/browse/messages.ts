/** Strings of the repository browse page. */
export const browseMessages = {
  browse: {
    treeLabel: 'Content of {name}',
    detailsLabel: 'Details of the selection',
    loadMore: {
      one: 'Load more ({count} entry loaded)',
      other: 'Load more ({count} entries loaded)',
    },
    servedBy: 'Served by',
    component: 'Component',
    detailFailed: 'The details could not be loaded',
    loadFailed: 'The content could not be loaded',
    folderPath: 'folder path',
    folderHint: 'Open a file in the tree to see its size, checksums and download link.',
    nothingSelected: 'Nothing selected',
    nothingSelectedBody: 'Choose a file or a folder in the tree to see its details here.',
    noTreeTitle: 'This repository has no file tree',
    noTreeBody:
      'Docker and OCI content is stored as images and tags, not as paths. Open the component list to browse it.',
    openComponents: 'Open the component list',
    itemGone: 'That file was removed meanwhile. The tree was refreshed.',
    pathLabel: 'Content path',
    filterLabel: 'Name starts with',
    filterPlaceholder: 'com, lib-1',
    emptyTitle: 'This repository is empty',
    emptyHosted: 'Publish something with one of the clients below and it appears here.',
    emptyOther: 'Nothing is cached or visible here yet.',
    noMatch: 'No entry at the top level starts with "{q}".',
  },
} as const
