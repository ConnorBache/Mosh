import { useEffect, useRef, useState, type ReactNode } from 'react'
import { open } from '@tauri-apps/plugin-dialog'
import { ArrowLeft, ArrowRight, ArrowUpRight, Bookmark, ChevronRight, Clock3, Database, File, FileText, Folder, FolderInput, FolderPlus, HardDrive, Layers3, ListFilter, Network, Plus, RefreshCw, Search, Tags, X } from 'lucide-react'
import { bytes, displayDate, leaf, ledger, type Details, type LedgerObject, type Saved } from './ledger'
import { Cleave, Collections, Disk, History, Inspector, TagExplorer } from './Views'

const navigation = [['Explorer', Folder], ['Tags', Tags], ['Collections', Network], ['Disk usage', HardDrive], ['Cleave', Layers3], ['History', Clock3]] as const
type View = typeof navigation[number][0]
export interface Field { title: string; label: string; apply: (text: string) => Promise<void> }
export type Task = <T>(name: string, action: () => Promise<T>) => Promise<T | undefined>
const limit = 100

export function App() {
  const [view, setView] = useState<View>('Explorer')
  const [roots, setRoots] = useState<string[]>([])
  const [saved, setSaved] = useState<Saved[]>([])
  const [trail, setTrail] = useState<LedgerObject[]>([])
  const [query, setQuery] = useState('')
  const [draft, setDraft] = useState('')
  const [sort, setSort] = useState('name')
  const [offset, setOffset] = useState(0)
  const [rows, setRows] = useState<LedgerObject[]>([])
  const [marked, setMarked] = useState<string[]>([])
  const [focused, setFocused] = useState<LedgerObject | null>(null)
  const [generation, setGeneration] = useState(0)
  const [loading, setLoading] = useState(true)
  const [working, setWorking] = useState('')
  const [error, setError] = useState('')
  const [notice, setNotice] = useState('')
  const [field, setField] = useState<Field | null>(null)
  const searchInput = useRef<HTMLInputElement>(null)
  const current = trail.at(-1)
  const physicalRoot = trail.find((object) => object.path)?.path ?? roots[0] ?? ''
  const refresh = () => setGeneration((value) => value + 1)
  const fail = (error: unknown) => setError(String(error))
  const resetSelection = () => { setFocused(null); setMarked([]); setOffset(0) }

  useEffect(() => {
    let active = true
    Promise.all([ledger<string[]>('roots'), ledger<Saved[]>('saved')]).then(([directories, searches]) => {
      if (active) { setRoots(directories); setSaved(searches) }
    }).catch((error) => { if (active) fail(error) })
    return () => { active = false }
  }, [generation])

  useEffect(() => {
    let active = true
    setLoading(true)
    ledger<LedgerObject[]>('search', { query, options: { parent: current?.id, limit: limit + 1, offset, sort } })
      .then((rows) => { if (active) setRows(rows) })
      .catch((error) => { if (active) { setRows([]); fail(error) } })
      .finally(() => { if (active) setLoading(false) })
    return () => { active = false }
  }, [generation, current?.id, query, offset, sort])

  useEffect(() => {
    function onKey(event: KeyboardEvent) {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'k') {
        event.preventDefault(); searchInput.current?.focus()
      }
      if (event.key === 'Escape') { setFocused(null); setMarked([]); setField(null) }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])

  function browse(object: LedgerObject, first = false) {
    setTrail((value) => first ? [object] : [...value, object])
    setQuery(''); setDraft(''); setView('Explorer'); resetSelection()
  }

  function search(text: string) {
    setTrail([]); setQuery(text); setDraft(text); setView('Explorer'); setError(''); resetSelection()
  }

  async function enter(reference: string) {
    try { browse((await ledger<Details>('inspect', { reference })).object, true) } catch (error) { fail(error) }
  }

  const task: Task = async (name, action) => {
    setError(''); setNotice(''); setWorking(name)
    try { return await action() }
    catch (error) { fail(error); return undefined }
    finally { setWorking('') }
  }

  async function scan(path?: string) {
    try {
      const chosen = path ?? await open({ directory: true, title: 'Choose a directory to index' })
      if (typeof chosen !== 'string') return
      const report = await task('Indexing directory…', () => ledger<{ scanned: number; errors: number }>('scan', { path: chosen }))
      if (report) {
        setNotice('Indexed ' + report.scanned.toLocaleString() + ' objects' + (report.errors ? ' · ' + report.errors + ' reported errors' : '') + '.')
        refresh(); await enter(chosen)
      }
    } catch (error) { fail(error) }
  }

  function openObject(item: LedgerObject) {
    if (item.reparse) { setFocused(item); return }
    if (item.kind !== 'file') browse(item)
    else ledger('open', { reference: item.id }).catch(fail)
  }

  return <div className="app">
    <aside className="sidebar">
      <div className="brand"><span className="brand-mark"><Folder size={20} strokeWidth={2.3} /></span>Mosh</div>
      <nav aria-label="Workspace">{navigation.map(([name, Icon]) =>
        <button key={name} className={'nav-item' + (view === name ? ' active' : '')}
          aria-current={view === name ? 'page' : undefined} onClick={() => setView(name)}><Icon size={17} />{name}</button>)}</nav>
      <section className="side-section">
        <div className="side-label"><h2>Indexed folders</h2><button className="icon-button" aria-label="Index a directory" disabled={!!working} onClick={() => scan()}><Plus size={16} /></button></div>
        {roots.map((root) => <button className={'root-item' + (trail[0]?.path === root ? ' active-root' : '')} key={root} title={root} onClick={() => enter(root)}>
          <Folder size={15} /><span>{leaf(root)}</span></button>)}
        {!roots.length && <p className="side-note">Choose a folder to get started.</p>}
      </section>
      {!!saved.length && <section className="side-section"><div className="side-label"><h2>Saved searches</h2></div>
        {saved.map((item) => <button className="root-item" key={item.name} onClick={() => search(item.query)}><Bookmark size={15} /><span>{item.name}</span></button>)}</section>}
      <div className="sidebar-foot"><Database size={15} /> Local Ledger <span className="online" aria-label="Local" /></div>
    </aside>
    <main className="main">
      <div className="topbar">
        <form className="search" onSubmit={(event) => { event.preventDefault(); search(draft) }}>
          <Search size={18} /><input ref={searchInput} aria-label="Search the Ledger" placeholder="Search names, tags, or contents…" value={draft} onChange={(event) => setDraft(event.target.value)} />
          {draft && <button className="icon-button" type="button" aria-label="Clear search" onClick={() => search('')}><X size={15} /></button>}<kbd>Ctrl K</kbd>
        </form>
        <button className="primary" disabled={!!working} onClick={() => scan()}><FolderInput size={17} /> Index folder</button>
      </div>
      <div className="alerts" aria-live="polite">
        {error && <div className="alert error"><span>{error}</span><button aria-label="Dismiss error" onClick={() => setError('')}><X size={16} /></button></div>}
        {notice && <div className="alert"><span>{notice}</span><button aria-label="Dismiss message" onClick={() => setNotice('')}><X size={16} /></button></div>}
        {working && <div className="alert"><span>{working}</span><button onClick={() => ledger('cancel').catch(fail)}>Cancel</button></div>}
      </div>
      {view === 'Explorer' && <div className="workspace">
        <section className="area grow">
          <div className="breadcrumb"><button onClick={() => search('')}>Ledger</button>
            {trail.map((item, index) => <span key={item.id + index}><ChevronRight size={13} /><button onClick={() => { setTrail(trail.slice(0, index + 1)); resetSelection() }}>{item.name}</button></span>)}
            {query && <span><ChevronRight size={13} />Search</span>}</div>
          <div className="heading"><div><h1>{query ? 'Search results' : current?.name ?? 'Explorer'}</h1>
            {query && <p className="query-caption">{query}</p>}{current?.path && <p className="path">{current.path}</p>}</div>
            {current?.path && <button className="secondary" disabled={!!working} onClick={() => scan(physicalRoot)}><RefreshCw size={16} /> Rescan</button>}</div>
          <div className="toolbar">
            {query && <button className="secondary" onClick={() => setField({ title: 'Save search', label: 'Search name', apply: async (name) => { await ledger('save', { name, query }); refresh() } })}><Bookmark size={15} /> Save</button>}
            {current?.path && <button className="secondary" onClick={() => setField({ title: 'New folder', label: 'Folder name', apply: async (name) => { await ledger('folder', { reference: current.id, name }); refresh() } })}><FolderPlus size={16} /> New folder</button>}
            {!!marked.length && <button className="secondary" onClick={() => setView('Cleave')}><Layers3 size={16} /> Cleave selection ({marked.length})</button>}
            <div className="toolbar-end"><ListFilter size={16} /><select aria-label="Sort by" value={sort} onChange={(event) => { setSort(event.target.value); setOffset(0) }}>
              <option value="name">Name (A–Z)</option><option value="name_desc">Name (Z–A)</option><option value="modified">Recently modified</option><option value="size">Largest first</option><option value="path">Path</option>
            </select></div>
          </div>
          {rows.length ? <>
            <div className="file-table">
              <div className="file-head"><input type="checkbox" aria-label="Select all visible files" checked={rows.slice(0, limit).every((item) => marked.includes(item.id))}
                onChange={(event) => setMarked(event.target.checked ? rows.slice(0, limit).map((item) => item.id) : [])} /><span>Name</span><span>Modified</span><span>Size</span></div>
              {rows.slice(0, limit).map((item) => <div key={item.id} className={'file-row' + (focused?.id === item.id ? ' selected' : '')}>
                <input type="checkbox" aria-label={'Select ' + item.name} checked={marked.includes(item.id)} onChange={(event) =>
                  setMarked((value) => event.target.checked ? [...value, item.id] : value.filter((id) => id !== item.id))} />
                <button className="filename" title={item.path ?? item.name} onClick={() => setFocused(item)} onDoubleClick={() => openObject(item)} onKeyDown={(event) => {
                  if (event.key === 'Enter') { event.preventDefault(); openObject(item) }
                }}><ObjectIcon item={item} /><span>{item.name}</span>
                  {item.git_status && item.git_status !== 'tracked' && <small>{item.git_status}</small>}</button>
                <span>{item.modified === null ? '—' : displayDate(item.modified)}</span><span>{item.kind === 'file' ? bytes(item.size) : '—'}</span>
              </div>)}</div>
            <div className="pagination"><span>{offset + 1}–{offset + Math.min(rows.length, limit)}</span>
              <button className="icon-button" aria-label="Previous page" disabled={!offset} onClick={() => setOffset(offset - limit)}><ArrowLeft size={16} /></button>
              <button className="icon-button" aria-label="Next page" disabled={rows.length <= limit} onClick={() => setOffset(offset + limit)}><ArrowRight size={16} /></button></div>
          </> : <Empty icon={<Folder size={31} />} title={loading ? 'Reading the Ledger…' : query ? 'No matching objects' : current ? 'This collection is empty' : 'Choose a folder to index'}
            text={!loading && !roots.length && !query ? 'Browse and search its files in place.' : undefined}>
            {!loading && !query && !current && <button className="primary" onClick={() => scan()} disabled={!!working}><Plus size={17} /> Index a folder</button>}</Empty>}
        </section>
        {focused && <Inspector key={focused.id} item={focused} generation={generation} refresh={refresh} fail={fail} close={() => setFocused(null)} show={() => openObject(focused)} />}
      </div>}
      {view === 'Tags' && <TagExplorer generation={generation} search={search} fail={fail} />}
      {view === 'Collections' && <Collections generation={generation} refresh={refresh} browse={(object) => browse(object, true)} field={setField} fail={fail} />}
      {view === 'Disk usage' && <Disk roots={roots} initial={physicalRoot} task={task} busy={!!working} fail={fail} />}
      {view === 'Cleave' && <Cleave initial={query} marked={marked} refresh={refresh} task={task} busy={!!working} />}
      {view === 'History' && <History generation={generation} fail={fail} />}
    </main>
    {field && <Dialog {...field} close={() => setField(null)} />}
  </div>
}

export function ObjectIcon({ item }: { item: LedgerObject }) {
  const Icon = item.kind === 'marriage' ? Layers3 : item.kind === 'coll' ? Folder : item.mime?.startsWith('text') ? FileText : File
  return <Icon size={19} className={item.kind === 'file' ? 'file-icon' : 'folder-icon'} />
}

export function Empty({ icon, title, text, children }: { icon: ReactNode; title: string; text?: string; children?: ReactNode }) {
  return <div className="empty"><div className="empty-icon">{icon}</div><h2>{title}</h2>{text && <p>{text}</p>}{children}</div>
}

export function OpenButton({ item, show }: { item: LedgerObject; show: () => void }) {
  return !item.reparse && <button className="secondary" onClick={show}>{item.kind === 'file' ? 'Open file' : 'Browse collection'} <ArrowUpRight size={15} /></button>
}

function Dialog({ title, label, apply, close }: Field & { close: () => void }) {
  const [text, setText] = useState('')
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
  return <div className="overlay"><form className="dialog" role="dialog" aria-label={title} aria-modal="true" onSubmit={async (event) => {
    event.preventDefault(); setBusy(true)
    try { await apply(text.trim()); close() } catch (error) { setError(String(error)); setBusy(false) }
  }}>
    <div className="heading"><h2>{title}</h2><button type="button" className="icon-button" aria-label="Close dialog" onClick={close}><X size={17} /></button></div>
    <label>{label}<input required autoFocus value={text} onChange={(event) => setText(event.target.value)} /></label>
    {error && <p role="alert">{error}</p>}<button className="primary" disabled={busy || !text.trim()}>Save</button>
  </form></div>
}
