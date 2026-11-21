// main.ts — launcher for the statically-linked Rust forge binary (./forge).
//
// The pod runs `node /bot/main.ts`. The binary itself owns the env lifecycle
// (bind an ephemeral port, POST $MAKEFUN_ENV_URL/register, poll GET /done,
// exit 0 when the trial is done), exactly like the kit starter's main.ts;
// this file only starts it and forwards its exit code. Without
// MAKEFUN_ENV_URL (local dev) the binary listens on 127.0.0.1:<port>
// (argv[2], $PORT, or 8787) so the kit's local runner can drive it.
//
// Written in plain JavaScript syntax inside a .ts file so it runs under any
// Node 22 type-stripping mode.
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const dir = path.dirname(fileURLToPath(import.meta.url));
const bin = path.join(dir, 'forge');
const envUrl = (process.env.MAKEFUN_ENV_URL ?? '').trim();
const args = envUrl === '' ? [process.argv[2] ?? process.env.PORT ?? '8787'] : [];
const child = spawn(bin, args, { stdio: 'inherit', env: process.env });
child.on('error', (err) => {
  process.stderr.write(`[entry] cannot start forge: ${err.message}\n`);
  process.exit(1);
});
child.on('exit', (code) => {
  process.exit(code ?? 1);
});
