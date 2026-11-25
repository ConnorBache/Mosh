import assert from 'node:assert/strict'
import { spawn, execFileSync } from 'node:child_process'
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { once } from 'node:events'
import { DatabaseSync } from 'node:sqlite'
import { chromium } from 'playwright-core'

const [application, cli, legacy] = process.argv.slice(2).map((path) => resolve(path))
assert(application && cli, 'Pass the installed desktop executable and the CLI executable')
const fixture = await mkdtemp(join(tmpdir(), 'mosh-desktop-test-'))
const version = (path) => {
  const sqlite = new DatabaseSync(path, { readOnly: true })
  try { return sqlite.prepare('PRAGMA user_version').get().user_version }
  finally { sqlite.close() }
}
let app
let browser
try {
  const root = join(fixture, 'Sample workspace')
  const db = join(fixture, 'ledger.sqlite3')
  await mkdir(root)
  await mkdir(join(root, 'Archive'))
  await writeFile(join(root, 'Orbit.md'), 'A test report about lunar navigation.')
  await writeFile(join(root, 'Copy.md'), 'A test report about lunar navigation.')
  await writeFile(join(root, 'Archive', 'Notes.txt'), 'A résumé about altitude.')
  const oldDb = legacy ? join(fixture, 'old-source.sqlite3') : db
  execFileSync(legacy ?? cli, ['--db', oldDb, 'scan', root], { windowsHide: true })
  execFileSync(legacy ?? cli, ['--db', oldDb, 'tag', 'add', join(root, 'Orbit.md'), 'space-research'], { windowsHide: true })
  if (legacy) {
    assert.equal(version(oldDb), 1)
    execFileSync(legacy, ['--db', oldDb, 'backup', db], { windowsHide: true })
    assert.equal(version(db), 1)
  }

  const server = createServer()
  server.listen(0, '127.0.0.1')
  await once(server, 'listening')
  const port = server.address().port
  await new Promise((done) => server.close(done))
  const userData = join(fixture, 'WebView')
  const webviewLog = join(fixture, 'webview.log')
  app = spawn(application, [], {
    env: { ...process.env, MOSH_DB: db, WEBVIEW2_USER_DATA_FOLDER: userData,
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: '--remote-debugging-port=' + port + ' --enable-logging --log-file="' + webviewLog + '"' },
    stdio: 'inherit',
  })
  let lastError
  const deadline = Date.now() + 35000
  while (!browser && Date.now() < deadline) {
    if (app.exitCode !== null) throw new Error('The desktop executable exited before opening its WebView')
    try { browser = await chromium.connectOverCDP('http://127.0.0.1:' + port, { timeout: 1500 }) }
    catch (error) { lastError = error; await new Promise((done) => setTimeout(done, 250)) }
  }
  if (!browser) {
    const data = await readdir(userData).catch(() => [])
    const log = await readFile(webviewLog, 'utf8').catch(() => '')
    throw new Error('Could not attach to the installed WebView2. Profile entries: ' + data.join(', ') +
      '\n' + log.slice(-3000), { cause: lastError })
  }
  const context = browser.contexts()[0]
  const page = context.pages()[0] ?? await context.waitForEvent('page')
  page.setDefaultTimeout(8000)
  const nav = (name) => page.getByRole('navigation', { name: 'Workspace' }).getByRole('button', { name, exact: true })
  await page.locator('.sidebar').getByRole('button', { name: 'Sample workspace', exact: true }).click()
  await page.getByRole('heading', { name: 'Sample workspace' }).waitFor()
  if (legacy) {
    assert.equal(version(db), 8)
    assert.equal(version(oldDb), 1)
  }
  await page.getByRole('button', { name: 'Orbit.md', exact: true }).click()
  const inspector = page.getByRole('complementary', { name: 'Metadata inspector' })
  await inspector.getByText('space-research', { exact: true }).waitFor()
  await inspector.getByRole('button', { name: 'Individuality', exact: true }).click()
  await inspector.getByRole('button', { name: 'Review vocabulary', exact: true }).waitFor()
  await inspector.getByRole('button', { name: 'Preview', exact: true }).click()
  await inspector.getByText('A test report about lunar navigation.').waitFor()
  await inspector.getByRole('button', { name: 'Details', exact: true }).click()
  await inspector.getByRole('textbox', { name: 'Paint notes' }).fill('Verified through the installed desktop')
  await inspector.getByRole('button', { name: 'Save', exact: true }).click()
  await inspector.getByText('Paint saved').waitFor()
  await page.getByRole('button', { name: 'Close inspector' }).click()

  const search = page.getByRole('textbox', { name: 'Search the Ledger' })
  await search.fill('paint:verified')
  await search.press('Enter')
  await page.getByRole('heading', { name: 'Search results' }).waitFor()
  await page.getByRole('button', { name: 'Orbit.md', exact: true }).waitFor()
  await search.fill('content:resume')
  await search.press('Enter')
  await page.getByRole('button', { name: 'Notes.txt', exact: true }).waitFor()

  await nav('Tags').click()
  await page.getByRole('heading', { name: 'Tag Explorer' }).waitFor()
  await page.getByRole('textbox', { name: 'Filter tag catalog' }).fill('space-research')
  await page.getByRole('button', { name: 'Include', exact: true }).click()
  await page.getByRole('button', { name: 'Search selection' }).click()
  await page.getByRole('button', { name: 'Orbit.md', exact: true }).waitFor()

  await nav('Collections').click()
  await page.getByRole('button', { name: 'Marriage', exact: true }).click()
  const newMarriage = page.getByRole('dialog', { name: 'New Marriage' })
  await newMarriage.getByRole('textbox').fill('Navigation research')
  await newMarriage.getByRole('button', { name: 'Save' }).click()
  await page.getByRole('button', { name: 'Add reference' }).click()
  const relationship = page.getByRole('dialog', { name: 'Add a relationship' })
  await relationship.getByRole('textbox').fill(join(root, 'Orbit.md'))
  await relationship.getByRole('button', { name: 'Save' }).click()
  await page.getByRole('button', { name: 'Orbit.md', exact: true }).waitFor()

  await nav('Disk usage').click()
  await page.getByRole('heading', { name: 'Storage by location' }).waitFor()
  await page.getByRole('button', { name: 'Measure Ledger' }).click()
  await page.getByText('file records', { exact: true }).waitFor()
  await page.getByRole('button', { name: 'Find duplicates' }).click()
  await page.getByText('2 paths · 2 files', { exact: false }).waitFor()

  await nav('Cleave').click()
  await page.getByRole('textbox', { name: 'Ledger query' }).fill('name:Orbit.md')
  await page.getByRole('combobox').selectOption('quarantine')
  await page.getByRole('button', { name: 'Preview', exact: true }).click()
  await page.getByRole('heading', { name: 'Manifest' }).waitFor()
  const execute = page.getByRole('button', { name: 'Execute plan' })
  assert(await execute.isDisabled())
  await page.getByRole('checkbox', { name: 'Confirm the displayed file changes' }).check()
  await execute.click()
  await page.getByText('Plan done', { exact: false }).waitFor()
  await assert.rejects(readFile(join(root, 'Orbit.md')), { code: 'ENOENT' })
  await page.getByRole('checkbox', { name: 'Confirm the displayed file changes' }).check()
  await page.getByRole('button', { name: 'Undo plan' }).click()
  await page.getByText('Plan undone', { exact: false }).waitFor()
  assert.match(await readFile(join(root, 'Orbit.md'), 'utf8'), /lunar navigation/)

  await nav('History').click()
  await page.getByRole('textbox', { name: 'Filter activity' }).fill('cleave undone')
  await page.getByText('cleave undone', { exact: true }).waitFor()

  await nav('Settings').click()
  const tierTwo = page.getByRole('group', { name: 'T2', exact: true })
  assert.equal(await tierTwo.getByLabel('Minimum tags').inputValue(), '5')
  await tierTwo.getByLabel('Minimum tags').fill('6')
  await page.getByRole('button', { name: 'Save tiers' }).click()
  await page.getByRole('status').filter({ hasText: 'Tiers saved' }).waitFor()
  await nav('Agents').click()
  await page.getByRole('heading', { name: 'Agents', exact: true }).waitFor()
  await nav('Ask Mosh').click()
  await page.getByRole('textbox', { name: 'What are you looking for?' }).fill('Find Orbit')
  await page.locator('.intel').getByRole('button', { name: 'Search', exact: true }).click()
  await page.getByRole('button', { name: 'Orbit.md', exact: true }).waitFor()
  const childRoot = join(root, 'Telemetry')
  const childDb = join(fixture, 'child.sqlite3')
  await mkdir(childRoot)
  await writeFile(join(childRoot, 'child-scope.txt'), 'Generated sensor calibration fixture.')
  const call = (ledgerPath, args) => JSON.parse(execFileSync(cli, ['--db', ledgerPath, ...args], { windowsHide: true, encoding: 'utf8' }))
  call(childDb, ['scan', childRoot])
  call(childDb, ['scope', 'allow', call(db, ['scope', 'info']).mosh, 'local'])
  await nav('Nested Moshes').click()
  await page.getByLabel('Child Ledger path').fill(childDb)
  await page.getByLabel('Child root directory').fill(childRoot)
  await page.getByLabel('Display name').fill('Telemetry child')
  await page.getByRole('button', { name: 'Attach child', exact: true }).click()
  await page.getByRole('button', { name: 'Telemetry child', exact: true }).click()
  await page.getByLabel('Search this child').fill('name:child-scope.txt')
  await page.getByRole('button', { name: 'Search child', exact: true }).click()
  await page.getByText('child-scope.txt', { exact: true }).waitFor()
  assert.equal(call(db, ['search', 'name:child-scope.txt']).length, 0)
  console.log(`Windows desktop scope, Individuality, and filesystem smoke passed${legacy ? ' on an upgraded format 1 copy' : ''}.`)
} finally {
  await browser?.close().catch(() => {})
  if (app?.exitCode === null && app.pid && app.signalCode === null) {
    const exited = once(app, 'exit')
    app.kill()
    await exited
  }
  await rm(fixture, { recursive: true, force: true, maxRetries: 20, retryDelay: 250 })
}
