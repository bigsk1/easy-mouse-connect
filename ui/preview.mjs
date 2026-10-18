import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { extname, resolve, sep } from 'node:path';

const root = fileURLToPath(new URL('.', import.meta.url));
const portIndex = process.argv.indexOf('--port');
const port = Number(portIndex >= 0 ? process.argv[portIndex + 1] : process.env.PORT || 5173);
const mime = { '.html': 'text/html; charset=utf-8', '.css': 'text/css; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.svg': 'image/svg+xml' };

if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error('Provide a valid TCP port with --port.');

createServer(async (request, response) => {
  try {
    const pathname = decodeURIComponent(new URL(request.url, 'http://localhost').pathname);
    const path = resolve(root, pathname === '/' ? 'index.html' : `.${pathname}`);
    if (!path.startsWith(root.endsWith(sep) ? root : `${root}${sep}`) || !['GET', 'HEAD'].includes(request.method)) {
      response.writeHead(403); response.end('Forbidden'); return;
    }
    const data = await readFile(path);
    response.writeHead(200, {
      'Content-Type': mime[extname(path)] || 'application/octet-stream',
      'Content-Security-Policy': "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'none'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'",
      'X-Content-Type-Options': 'nosniff',
      'Cache-Control': 'no-store',
    });
    response.end(request.method === 'HEAD' ? undefined : data);
  } catch {
    response.writeHead(404); response.end('Not found');
  }
}).listen(port, '127.0.0.1', () => {
  console.log(`Easy Mouse Connect browser preview: http://127.0.0.1:${port}`);
  console.log('The preview has no native connection, input, clipboard, or file engine.');
});
