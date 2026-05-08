// Quiet SessionStart hook for Codex and Claude.
//
// It performs an incremental Memory OS import check every time an agent
// session starts, without replaying the full corpus and without injecting
// command output into the model context.
const { spawnSync } = require('node:child_process');

try {
  spawnSync('munin', ['memory-os', 'ingest', '--format', 'json'], {
    env: {
      ...process.env,
      MUNIN_MEMORY_OS_FORCE_ONBOARDING: '1',
    },
    stdio: 'ignore',
    timeout: 120000,
    windowsHide: true,
  });
} catch (_) {
  // Startup memory refresh is best-effort. Read surfaces fail with concrete
  // errors if the importer is truly broken.
}

