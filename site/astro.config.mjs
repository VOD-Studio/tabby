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
      sidebar: [
        { slug: 'docs' },
        { slug: 'docs/install' },
        {
          label: '快速开始',
          items: [{ slug: 'docs/quick-start/connect' }, { slug: 'docs/quick-start/managed' }],
        },
        {
          label: '页面指南',
          items: [
            { slug: 'docs/use/overview' }, { slug: 'docs/use/proxies' },
            { slug: 'docs/use/providers' }, { slug: 'docs/use/rules' },
            { slug: 'docs/use/connections' }, { slug: 'docs/use/logs' },
            { slug: 'docs/use/settings' }, { slug: 'docs/use/subscriptions' },
          ],
        },
        {
          label: '托管内核',
          items: [
            { slug: 'docs/manage/kernel' }, { slug: 'docs/manage/subscriptions' },
            { slug: 'docs/manage/dns' }, { slug: 'docs/manage/system-integration' },
          ],
        },
        {
          label: '参考',
          items: [
            { slug: 'docs/reference/configuration' },
            { slug: 'docs/reference/keys' },
            { slug: 'docs/reference/compatibility' },
          ],
        },
        { slug: 'docs/troubleshooting' },
      ],
    }),
  ],
});
