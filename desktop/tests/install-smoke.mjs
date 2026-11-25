import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdtemp, readdir, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const [installer, cli, legacy] = process.argv.slice(2).map(path => resolve(path))
assert(installer && cli, 'Pass the Windows installer and CLI executable')
const key = String.raw`HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Mosh`
try {
  execFileSync('reg.exe', ['query', key], { windowsHide: true, stdio: 'ignore' })
  throw new Error('Mosh is already installed for this user. Use a disposable Windows account.')
} catch (error) {
  if (error.status !== 1) throw error
}
const directory = await mkdtemp(join(tmpdir(), 'mosh-preview-install-'))
try {
  execFileSync(installer, ['/S', '/D=' + directory], { windowsHide: true, timeout: 120_000 })
  const smoke = fileURLToPath(new URL('./smoke.mjs', import.meta.url))
  for (const extra of legacy ? [[], [legacy]] : [[]]) {
    execFileSync(process.execPath, [smoke, join(directory, 'mosh-desktop.exe'), cli, ...extra],
      { stdio: 'inherit', windowsHide: true, timeout: 120_000 })
  }
} finally {
  try {
    const uninstall = (await readdir(directory)).find(name => /^uninstall.*\.exe$/i.test(name))
    if (uninstall) execFileSync(join(directory, uninstall), ['/S'], { windowsHide: true, timeout: 120_000 })
  } finally {
    await rm(directory, { recursive: true, force: true, maxRetries: 40, retryDelay: 250 })
  }
}
