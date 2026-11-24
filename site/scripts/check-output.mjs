import { fileURLToPath } from 'node:url';
import config from '../astro.config.mjs';

const dist = new URL('../dist/', import.meta.url);
const root = new URL(`${config.base.replace(/\/$/, '')}/`, config.site);
const files = new Set(await Array.fromAsync(new Bun.Glob('**/*').scan({ cwd: fileURLToPath(dist) })));
const pages = new Map();
const errors = [];

function fileFor(url) {
  const path = decodeURIComponent(url.pathname).slice(root.pathname.length);
  if (path.endsWith('/') || path === '') return `${path}index.html`;
  return files.has(path) ? path : `${path}/index.html`;
}

function checkReference(file, source, reference) {
  const url = new URL(reference, source);
  if (url.origin !== root.origin) return;
  if (!url.pathname.startsWith(root.pathname)) {
    errors.push(`${file}: ${reference} must stay under ${root.pathname}`);
    return;
  }
  const target = fileFor(url);
  if (!files.has(target)) {
    errors.push(`${file}: ${reference} does not resolve to a built file`);
    return;
  }
  const anchor = decodeURIComponent(url.hash.slice(1));
  if (anchor && pages.has(target) && !pages.get(target).ids.has(anchor)) {
    errors.push(`${file}: ${reference} points to a missing anchor`);
  }
}

for (const file of files) {
  if (!file.endsWith('.html')) continue;
  const ids = new Set();
  const references = new Set();
  const parser = new HTMLRewriter()
    .on('[id]', { element: (el) => ids.add(el.getAttribute('id')) })
    .on('[href], [src], [srcset]', {
      element(el) {
        for (const attribute of ['href', 'src']) {
          if (el.hasAttribute(attribute)) references.add(el.getAttribute(attribute));
        }
        for (const candidate of (el.getAttribute('srcset') ?? '').split(',')) {
          const path = candidate.trim().split(/\s+/)[0];
          if (path) references.add(path);
        }
      },
    });
  const page = Bun.file(new URL(file, dist));
  await parser.transform(new Response(page)).text();
  const path = file.replace(/(^|\/)index\.html$/, '$1');
  pages.set(file, { source: new URL(path, root), ids, references });
}

for (const [file, { source, references }] of pages) {
  for (const reference of references) checkReference(file, source, reference);
}
for (const file of files) {
  if (!file.endsWith('.css') || file.startsWith('pagefind/')) continue;
  const stylesheet = await Bun.file(new URL(file, dist)).text();
  for (const match of stylesheet.matchAll(/url\((?:"([^"]*)"|'([^']*)'|([^)]*))\)/g)) {
    const reference = (match[1] ?? match[2] ?? match[3]).trim();
    if (reference && !reference.startsWith('#')) checkReference(file, new URL(file, root), reference);
  }
}

for (const file of ['index.html', 'download/index.html', 'docs/index.html', '404.html']) {
  if (!pages.has(file)) errors.push(`Missing required page: ${file}`);
}
const manifest = await Bun.file(new URL('pagefind/pagefind-entry.json', dist)).json();
const chinese = manifest.languages['zh-cn'];
if (!chinese?.page_count || !files.has(`pagefind/pagefind.${chinese.hash}.pf_meta`) || !files.has('pagefind/pagefind.js')) {
  errors.push('Missing the Chinese Pagefind index or its entry point');
}

if (errors.length) {
  process.exitCode = 1;
  console.error(errors.join('\n'));
} else {
  console.log(`Checked links and assets across ${pages.size} pages and a ${chinese.page_count}-page Chinese search index.`);
}
