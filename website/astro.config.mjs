import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

export default defineConfig({
  site: 'https://c9watch.mclee.dev',
  integrations: [
    starlight({
      title: 'c9watch',
      logo: {
        src: './public/icon.png',
      },
      social: [
        { icon: 'github', label: 'GitHub', href: 'https://github.com/minchenlee/c9watch' },
      ],
      customCss: [
        './src/styles/global.css',
      ],
      sidebar: [
        {
          label: 'Getting Started',
          items: [
            { label: 'Install', slug: 'install' },
          ],
        },
        {
          label: 'Reference',
          items: [
            { label: 'Features', slug: 'features' },
            { label: 'How It Works', slug: 'how-it-works' },
          ],
        },
        {
          label: 'Changelog',
          items: [
            { label: 'Changelog', slug: 'changelog' },
          ],
        },
      ],
      head: [
        {
          tag: 'meta',
          attrs: {
            property: 'og:image',
            content: 'https://c9watch.mclee.dev/og-image.png',
          },
        },
      ],
      pagination: true,
      tableOfContents: { minHeadingLevel: 2, maxHeadingLevel: 3 },
      expressiveCode: {
        themes: ['one-dark-pro'],
      },
    }),
  ],
});
