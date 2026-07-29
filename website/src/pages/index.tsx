import type {ReactNode} from 'react';
import clsx from 'clsx';
import Link from '@docusaurus/Link';
import useDocusaurusContext from '@docusaurus/useDocusaurusContext';
import Layout from '@theme/Layout';
import Heading from '@theme/Heading';

import styles from './index.module.css';

function HomepageHeader() {
  const {siteConfig} = useDocusaurusContext();
  return (
    <header className={clsx('hero hero--primary', styles.heroBanner)}>
      <div className="container">
        <Heading as="h1" className="hero__title">
          {siteConfig.title}
        </Heading>
        <p className="hero__subtitle">{siteConfig.tagline}</p>
        <div className={styles.buttons}>
          <Link
            className="button button--secondary button--lg"
            to="/docs/getting-started/installation">
            Get Started
          </Link>
          <Link
            className="button button--outline button--secondary button--lg"
            style={{marginLeft: '1rem'}}
            href="https://github.com/Callix-Tools/callix">
            GitHub
          </Link>
        </div>
      </div>
    </header>
  );
}

type FeatureItem = {
  title: string;
  description: string;
  badge: string;
};

const features: FeatureItem[] = [
  {
    title: 'Polyglot by design',
    badge: '🌐',
    description:
      'Python, TypeScript, Go, Rust, and PHP all normalize into one shared graph IR. Each language is a separate plugin registered through Python entry points.',
  },
  {
    title: 'Type-aware resolution',
    badge: '🎯',
    description:
      'Tree-sitter extracts structure and exact spans; a per-language resolver (ty, the TypeScript Compiler API, gopls, rust-analyzer, Intelephense) resolves real CALLS, REFERENCES, and HAS_TYPE edges.',
  },
  {
    title: 'Built for pipelines',
    badge: '⚙️',
    description:
      'Pure data producers, deterministic node IDs, round-trippable JSON, a strict mode for CI, a Neo4j exporter, and an MCP server for agents.',
  },
];

function Feature({title, description, badge}: FeatureItem) {
  return (
    <div className={clsx('col col--4')}>
      <div className="text--center" style={{fontSize: '3rem', marginBottom: '1rem'}}>
        {badge}
      </div>
      <div className="text--center padding-horiz--md">
        <Heading as="h3">{title}</Heading>
        <p>{description}</p>
      </div>
    </div>
  );
}

export default function Home(): ReactNode {
  const {siteConfig} = useDocusaurusContext();
  return (
    <Layout
      title={siteConfig.title}
      description="Extensible polyglot code analysis framework that parses source projects into a shared graph IR.">
      <HomepageHeader />
      <main>
        <section className={styles.features}>
          <div className="container">
            <div className="row" style={{marginTop: '2rem', marginBottom: '2rem'}}>
              {features.map((props, idx) => (
                <Feature key={idx} {...props} />
              ))}
            </div>
          </div>
        </section>
      </main>
    </Layout>
  );
}
