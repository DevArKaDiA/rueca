export default function compile(src: string): string {
  return minify(src);
}

function minify(src: string): string {
  return src.replace(/\s+/g, " ");
}
