import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

export default defineConfig({
  site: 'https://vod-studio.github.io',
  base: '/tabby',
  trailingSlash: 'always',
  devToolbar: { enabled: false },
  integrations: [
    starlight({
      title: 'Tabby',
      description: 'Tabby 是适用于 Linux 和 macOS 的中文 mihomo 终端控制台。',
      locales: { root: { label: '简体中文', lang: 'zh-CN' } },
      logo: { src: './src/assets/mascot.webp' },
      favicon: '/favicon.png',
      disable404Route: true,
      social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/VOD-Studio/tabby' }],
      customCss: ['@fontsource-variable/geist', './src/styles/tokens.css', './src/styles/docs.css'],
      sidebar: [{ slug: 'docs' }],
    }),
  ],
});
