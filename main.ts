// Forge lifecycle: bring the local forge up, tell the trial environment
// where it is listening, then stay alive until the environment says the
// trial is finished.
//
// The environment (the sealed suite) is the only client. It hands us its
// own origin through MAKEFUN_ENV_URL and expects exactly this dance:
//   1. bind an ephemeral port of our own choosing (port 0 lets the OS pick)
//   2. POST {"port": <that port>} to <env>/register
//   3. keep asking GET <env>/done until it answers {"done": true}
//   4. shut the server down and exit 0
// Port 8080 belongs to the environment, so we never bind it ourselves.
// Nothing here talks to any other service, and no request carries a clock
// reading — only the port number and the done flag travel on the wire.

import { createApp } from './app.ts';

// How long a single HTTP call to the environment may run before we give
// up on it and try again, and how the pauses between retries grow.
const CALL_BUDGET_MS = 30_000;
const FIRST_PAUSE_MS = 250;
const LONGEST_PAUSE_MS = 2_000;

const origin = (process.env.MAKEFUN_ENV_URL ?? '').replace(/\/+$/, '');
if (origin === '') {
  process.stderr.write('[forge] MAKEFUN_ENV_URL is missing, so there is no trial environment to join\n');
  process.exit(1);
}

function pause(ms: number): Promise<void> {
  return new Promise((done) => setTimeout(done, ms));
}

// Pause after `failures` failed attempts: double each time from the first
// pause, but never longer than the cap. The first few pauses are
// 250ms, 500ms, 1000ms, then 2000ms from then on.
function pauseAfter(failures: number): number {
  const doubled = FIRST_PAUSE_MS * 2 ** Math.min(failures, 3);
  return Math.min(LONGEST_PAUSE_MS, doubled);
}

// Announce our listening port to the environment, retrying until it is
// accepted. A 200 means the announcement landed; a 409 means some entry
// already announced first, which is not ours to fix, so we carry on.
// A 400 or 405 means we spoke the wrong protocol, which retrying cannot
// fix, so that is fatal. Anything else — a refused connection, a timeout,
// another status — just means the environment may still be starting, so
// we wait and try again.
async function announce(port: number): Promise<void> {
  let failures = 0;
  for (;;) {
    try {
      const answer = await fetch(`${origin}/register`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ port }),
        signal: AbortSignal.timeout(CALL_BUDGET_MS),
      });
      if (answer.status === 200 || answer.status === 409) return;
      if (answer.status === 400 || answer.status === 405) {
        throw new Error(`register of port ${port} refused with HTTP ${answer.status}: ${await answer.text()}`);
      }
    } catch (err) {
      if (err instanceof Error && err.message.startsWith('register refused')) throw err;
    }
    await pause(pauseAfter(failures));
    failures += 1;
  }
}

// Keep asking whether the trial is over. The environment answers at once
// with {"done": false} while the suite is still working, so this is a
// plain poll, not a held-open request. We never leave before it answers
// {"done": true}, however long that takes.
async function waitUntilDone(): Promise<void> {
  let failures = 0;
  for (;;) {
    try {
      const answer = await fetch(`${origin}/done`, {
        signal: AbortSignal.timeout(CALL_BUDGET_MS),
      });
      if (answer.ok) {
        const payload = (await answer.json()) as { done?: unknown };
        if (typeof payload === 'object' && payload !== null && payload.done === true) return;
      }
    } catch {
      // The environment is not answering yet, or this call timed out.
      // Either way the trial is not over, so we poll again.
    }
    await pause(pauseAfter(failures));
    failures += 1;
  }
}

async function runForge(): Promise<void> {
  const server = createApp();
  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, () => resolve());
  });
  const bound = server.address();
  if (bound === null || typeof bound === 'string') {
    throw new Error('the forge did not bind a TCP port of its own');
  }
  const port = bound.port;
  process.stdout.write(`[forge] listening on port ${port}\n`);

  await announce(port);
  process.stdout.write('[forge] announced; waiting for the trial to finish\n');

  await waitUntilDone();
  server.close();
  process.stdout.write('[forge] trial finished\n');
}

try {
  await runForge();
  process.exit(0);
} catch (err) {
  process.stderr.write(`[forge] ${err instanceof Error ? err.message : String(err)}\n`);
  process.exit(1);
}
