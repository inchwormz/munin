// Munin UserPromptSubmit hook.
//
// This hook is intentionally thin: all freshness and fallback policy lives in
// `munin runtime`. The hook only invokes the Munin CLI and passes the
// packet through as additional context when it returns in time.

const { spawnSync } = require('node:child_process');

const MAX_CONTEXT_CHARS = 8000;
const TIMEOUT_MS = 5000;

let input = '';
process.stdin.on('data', chunk => input += chunk);
process.stdin.on('end', () => {
  try {
    const data = JSON.parse(input || '{}');
    const rawPrompt = (data.prompt || data.user_message || data.message || '').trim();
    if (!rawPrompt) return;

    const packet = spawnSync('munin', ['runtime', '--surface', 'auto', '--format', 'prompt'], {
      encoding: 'utf8',
      timeout: TIMEOUT_MS,
      maxBuffer: 256 * 1024,
      windowsHide: true,
      cwd: process.cwd(),
    });

    if (packet.error || packet.status !== 0) return;
    let context = (packet.stdout || '').trim();
    if (!context) return;

    if (context.length > MAX_CONTEXT_CHARS) {
      context = `${context.slice(0, MAX_CONTEXT_CHARS)}\n<!-- truncated by munin-runtime -->`;
    }

    console.log(JSON.stringify({
      hookSpecificOutput: {
        hookEventName: 'UserPromptSubmit',
        additionalContext: `Munin runtime packet:\n${context}`,
      },
    }));
  } catch {
    // Prompt hooks must never block user input on hook parse failures.
  }
});
