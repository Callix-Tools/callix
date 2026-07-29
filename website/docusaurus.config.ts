import {themes as prismThemes} from 'prism-react-renderer';
import type {Config} from '@docusaurus/types';
import type * as Preset from '@docusaurus/preset-classic';

const config: Config = {
  title: 'callix',
  tagline: 'Polyglot code analysis in Rust — one install, resolvers included',
  favicon: 'img/logo.svg',

  future: {
    v4: true,
  },

  url: 'https://callix-tools.github.io',
  baseUrl: '/callix/',

  organizationName: 'Callix-Tools',
  projectName: 'callix',

  onBrokenLinks: 'throw',

  markdown: {
    hooks: {
      onBrokenMarkdownLinks: 'warn',
    },
  },

  i18n: {
    defaultLocale: 'en',
    locales: ['en'],
  },

  presets: [
    [
      'classic',
      {
        docs: {
          sidebarPath: './sidebars.ts',
          editUrl: 'https://github.com/Callix-Tools/callix/tree/main/website/',
          routeBasePath: 'docs',
        },
        blog: false,
        theme: {
          customCss: './src/css/custom.css',
        },
      } satisfies Preset.Options,
    ],
  ],

  themeConfig: {
    colorMode: {
      respectPrefersColorScheme: true,
    },
    navbar: {
      title: 'callix',
      logo: {
        alt: 'callix logo',
        src: 'img/logo.svg',
      },
      items: [
        {
          type: 'docSidebar',
          sidebarId: 'docsSidebar',
          position: 'left',
          label: 'Docs',
        },
        {
          to: '/docs/api-reference/callix',
          label: 'API',
          position: 'left',
        },
        {
          href: 'https://github.com/Callix-Tools/callix',
          label: 'GitHub',
          position: 'right',
        },
        {
          href: 'https://pypi.org/project/callix/',
          label: 'PyPI',
          position: 'right',
        },
      ],
    },
    footer: {
      style: 'dark',
      links: [
        {
          title: 'Docs',
          items: [
            {label: 'Introduction', to: '/docs/'},
            {label: 'Getting Started', to: '/docs/getting-started/installation'},
            {label: 'Guides', to: '/docs/guides/library-api'},
            {label: 'API Reference', to: '/docs/api-reference/callix'},
          ],
        },
        {
          title: 'Topics',
          items: [
            {label: 'Adapters', to: '/docs/adapters/overview'},
            {label: 'Resolvers', to: '/docs/adapters/resolvers'},
            {label: 'Graph Model', to: '/docs/graph-model/nodes'},
            {label: 'Cross-language', to: '/docs/guides/cross-language'},
          ],
        },
        {
          title: 'Links',
          items: [
            {
              label: 'GitHub',
              href: 'https://github.com/Callix-Tools/callix',
            },
            {
              label: 'PyPI',
              href: 'https://pypi.org/project/callix/',
            },
            {
              label: 'Issues',
              href: 'https://github.com/Callix-Tools/callix/issues',
            },
          ],
        },
      ],
      copyright: `Copyright © ${new Date().getFullYear()} callix. Built with Docusaurus.`,
    },
    prism: {
      theme: prismThemes.github,
      darkTheme: prismThemes.dracula,
      additionalLanguages: ['python', 'rust', 'go', 'bash', 'toml', 'json'],
    },
  } satisfies Preset.ThemeConfig,
};

export default config;
