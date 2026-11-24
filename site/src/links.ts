export const repository = 'https://github.com/VOD-Studio/tabby';
export const latestRelease = `${repository}/releases/latest`;

export function sitePath(path: `/${string}`) {
  return `${import.meta.env.BASE_URL.replace(/\/$/, '')}${path}`;
}
