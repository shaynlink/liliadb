import { spawn, spawnSync } from 'node:child_process';

export function command(binary, args, options = {}) {
  const result = spawnSync(binary, args, { encoding: 'utf8', timeout: 60_000, ...options });
  if (result.error) throw result.error;
  if (result.signal) throw new Error(`${binary} terminated: ${result.signal}`);
  return result;
}

export function service(binary, args) {
  const child = spawn(binary, args, { stdio: ['pipe', 'pipe', 'pipe'] });
  let diagnostic = '';
  child.stderr.on('data', chunk => { diagnostic = (diagnostic + chunk).slice(-8192); });
  const exited = new Promise(resolve => {
    child.once('error', error => resolve({ error }));
    child.once('close', code => resolve({ code }));
  });
  let failure;
  void exited.then(result => { failure = result; });
  return {
    child, exited,
    check() { if (failure) throw new Error(`service exited: ${JSON.stringify(failure)} ${diagnostic}`); },
    async close() {
      if (!failure) child.kill();
      await bounded(exited, 'service shutdown');
    },
  };
}

export async function bounded(promise, label) {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`${label} timed out`)), 15_000);
    })]);
  } finally { clearTimeout(timer); }
}
