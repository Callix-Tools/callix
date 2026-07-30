import type {SidebarsConfig} from '@docusaurus/plugin-content-docs';

const sidebars: SidebarsConfig = {
  docsSidebar: [
    {
      type: 'doc',
      id: 'intro',
      label: 'Introduction',
    },
    {
      type: 'category',
      label: 'Getting Started',
      items: [
        'getting-started/installation',
        'getting-started/quick-start',
        'getting-started/concepts',
      ],
    },
    {
      type: 'category',
      label: 'Guides',
      items: [
        'guides/library-api',
        'guides/querying',
        'guides/cross-language',
        'guides/custom-resolvers',
      ],
    },
    {
      type: 'category',
      label: 'Adapters',
      items: [
        'adapters/overview',
        'adapters/resolvers',
        'adapters/python',
        'adapters/typescript',
        'adapters/go',
        'adapters/rust',
        'adapters/php',
        'adapters/yaml',
      ],
    },
    {
      type: 'category',
      label: 'Graph Model',
      items: [
        'graph-model/nodes',
        'graph-model/relations',
        'graph-model/boundaries',
        'graph-model/serialization',
      ],
    },
    {
      type: 'category',
      label: 'API Reference',
      items: [
        'api-reference/callix',
        'api-reference/models',
        'api-reference/exceptions',
      ],
    },
    {
      type: 'category',
      label: 'Project',
      items: [
        'project/migrating-from-graphlens',
        'project/benchmarks',
        'project/thread-safety',
        'project/versioning',
      ],
    },
  ],
};

export default sidebars;
