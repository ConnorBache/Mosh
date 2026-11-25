import { useEffect, useState } from 'react'
import { Archive, ArrowDown, ArrowRight, ArrowUpRight, Clock3, GitBranch, HardDrive, Layers3, Network, Plus, Search, Tags, X } from 'lucide-react'
import { Empty, ObjectIcon, OpenButton, type Field, type Task } from './App'
import {
  bytes, displayDate, leaf, ledger, quoted, type Action, type Activity, type Commit, type Details, type DuplicateGroup,
  type Edge, type LedgerObject, type Manifest, type ResultPlan, type Space, type TagCount,
} from './ledger'

type Fail = (error: unknown) => void

export function Inspector({ item, generation, refresh, fail, close, show }: {
  item: LedgerObject; generation: number; refresh: () => void; fail: Fail; close: () => void; show: () => void
}) {
  const [tab, setTab] = useState('Details')
  const [detail, setDetail] = useState<Details | null>(null)
  const [tag, setTag] = useState('')
  const [filter, setFilter] = useState('')
  const [paint, setPaint] = useState('')
  const [level, setLevel] = useState('standard')
  const [locked, setLocked] = useState(false)
  const [saved, setSaved] = useState(false)
  const [preview, setPreview] = useState('')
  const [history, setHistory] = useState<Activity[]>([])
  const [commits, setCommits] = useState<Commit[]>([])
  const [diff, setDiff] = useState('')
  const reference = item.id

  useEffect(() => {
    let active = true
    ledger<Details>('inspect', { reference }).then((detail) => {
      if (active) { setDetail(detail); setPaint(detail.paint?.body ?? ''); setLevel(detail.paint?.level ?? 'standard'); setLocked(detail.paint?.locked ?? false) }
    }).catch((error) => { if (active) fail(error) })
    return () => { active = false }
  }, [reference, generation])

  useEffect(() => {
    if (tab === 'Preview') ledger<string>('preview', { reference }).then(setPreview).catch(fail)
    if (tab === 'History') {
      ledger<Activity[]>('history', { reference, limit: 50 }).then(setHistory).catch(fail)
      if (item.git_root) ledger<Commit[]>('git_history', { reference }).then(setCommits).catch(fail)
    }
  }, [tab, reference, generation, item.git_root])

  const object = detail?.object ?? item
  const tags = detail?.tags.filter((tag) => tag.name.includes(filter.toLowerCase())) ?? []
  return <aside className="inspector" aria-label="Metadata inspector">
    <div className="inspector-head"><ObjectIcon item={object} /><button className="icon-button" aria-label="Close inspector" onClick={close}><X size={18} /></button></div>
    <h2>{object.name}</h2><p className="path">{object.path ?? 'Virtual collection'}</p><OpenButton item={object} show={show} />
    <div className="tabs">{['Details', 'Preview', 'History'].map((name) =>
      <button key={name} className={tab === name ? 'current' : ''} onClick={() => setTab(name)}>{name}</button>)}</div>
    {tab === 'Details' && <>
      <dl className="metadata"><dt>Kind</dt><dd>{object.kind === 'file' ? object.mime ?? 'File' : object.kind === 'coll' ? 'Coll' : 'Marriage'}</dd>
        <dt>Modified</dt><dd>{displayDate(object.modified)}</dd><dt>Logical size</dt><dd>{bytes(object.size)}</dd><dt>Allocated</dt><dd>{bytes(object.allocated)}</dd>
        {object.git_root && <><dt>Git repository</dt><dd>{leaf(object.git_root)}</dd><dt>Git status</dt><dd>{object.git_status ?? 'Unavailable'}</dd></>}</dl>
      <section className="inspect-block"><h3>Tags</h3>
        {(detail?.tags.length ?? 0) > 18 && <input aria-label="Filter tags" placeholder="Filter tags" value={filter} onChange={(event) => setFilter(event.target.value)} />}
        <div className="chips">{tags.slice(0, 32).map((tag, index) => <span key={tag.name + index} className={'chip' + (tag.inherited ? ' inherited' : '')}
          title={(tag.inherited ? 'Inherited' : 'Direct') + ' · ' + tag.origin + (tag.evidence ? ' · ' + tag.evidence : '')}>
          {tag.name}{!tag.inherited && tag.origin === 'user' && <button aria-label={'Remove tag ' + tag.name}
            onClick={() => ledger('tag', { reference, tag: tag.name, add: false }).then(refresh).catch(fail)}><X size={12} /></button>}</span>)}</div>
        {tags.length > 32 && <p className="muted">Showing 32 of {tags.length} tags</p>}
        <form className="inline" onSubmit={(event) => {
          event.preventDefault(); ledger('tag', { reference, tag }).then(() => { setTag(''); refresh() }).catch(fail)
        }}><input aria-label="New tag" placeholder="Add a tag" value={tag} onChange={(event) => setTag(event.target.value)} />
          <button className="secondary" aria-label="Add tag" disabled={!tag.trim()}><Plus size={16} /></button></form>
      </section>
      <section className="inspect-block"><h3>Paint {detail?.paint?.stale && <small>Stale</small>}</h3>
        <form onSubmit={(event) => {
          event.preventDefault(); ledger('paint', { reference, level, body: paint, locked }).then(() => { setSaved(true); refresh() }).catch(fail)
        }}>
          <textarea aria-label="Paint notes" placeholder="Add notes for this object" value={paint} onChange={(event) => { setPaint(event.target.value); setSaved(false) }} />
          <div className="inline"><select aria-label="Paint level" value={level} onChange={(event) => { setLevel(event.target.value); setSaved(false) }}>
            {['none', 'minimal', 'standard', 'detailed'].map((name) => <option key={name}>{name}</option>)}</select>
            <label className="check"><input type="checkbox" checked={locked} onChange={(event) => { setLocked(event.target.checked); setSaved(false) }} /> Lock</label>
            <button className="secondary">Save</button></div>
          {saved && <p className="muted" role="status">Paint saved</p>}
        </form></section>
      <details className="inspect-block"><summary>Native and Ledger metadata</summary>
        <pre>{JSON.stringify({ ...object, owners: detail?.owners, windows_security: detail?.windows_security }, null, 2)}</pre></details>
    </>}
    {tab === 'Preview' && (preview ? <pre className="preview">{preview}</pre> : <p className="muted">No text preview is available.</p>)}
    {tab === 'History' && <section className="inspect-block">
      {history.map((record, index) => <div className="compact-event" key={index}><strong>{record.action}</strong><span>{displayDate(record.time)}</span></div>)}
      {!!commits.length && <h3><GitBranch size={15} /> Git history</h3>}
      {commits.map((commit) => <button key={commit.commit} className="commit"
        onClick={() => ledger<string>('git_diff', { reference, revision: commit.commit }).then(setDiff).catch(fail)}>
        <strong>{commit.subject}</strong><span>{commit.commit.slice(0, 7)} · {commit.author}</span></button>)}
      {diff && <pre>{diff}</pre>}
    </section>}
  </aside>
}

export function TagExplorer({ generation, search, fail }: { generation: number; search: (query: string) => void; fail: Fail }) {
  const [tags, setTags] = useState<TagCount[]>([])
  const [coverage, setCoverage] = useState<{ covered: number; objects: number }>()
  const [filter, setFilter] = useState('')
  const [selection, setSelection] = useState<Record<string, 'include' | 'exclude'>>({})
  useEffect(() => {
    Promise.all([ledger<TagCount[]>('tag_catalog', { limit: 1000 }), ledger<{ covered: number; objects: number }>('tag_coverage')])
      .then(([tags, coverage]) => { setTags(tags); setCoverage(coverage) }).catch(fail)
  }, [generation])
  const query = Object.entries(selection).map(([name, mode]) => (mode === 'exclude' ? 'NOT ' : '') + 'tag:' + quoted(name)).join(' AND ')
  function toggle(name: string, mode: 'include' | 'exclude') {
    setSelection((value) => {
      const next = { ...value }
      if (next[name] === mode) delete next[name]
      else next[name] = mode
      return next
    })
  }
  return <section className="area">
    <div className="heading"><div><h1>Tag Explorer</h1>{coverage && <p>{coverage.covered.toLocaleString()} of {coverage.objects.toLocaleString()} objects have 10 or more direct tags</p>}</div>
      <button className="primary" disabled={!query} onClick={() => search(query)}><Search size={16} /> Search selection</button></div>
    <div className="toolbar"><input aria-label="Filter tag catalog" placeholder="Filter tags" value={filter} onChange={(event) => setFilter(event.target.value)} />
      <span className="query-caption">{query}</span></div>
    {tags.length === 0 ? <Empty icon={<Tags size={28} />} title="No tags in the Ledger" /> : <div className="tag-grid">
      {tags.filter((tag) => tag.name.includes(filter.toLowerCase())).map((tag) => <article key={tag.name} className={'tag-card ' + (selection[tag.name] ?? '')}>
        <strong>{tag.name}</strong><span>{tag.count.toLocaleString()} objects</span><div className="inline">
          <button className="secondary" aria-pressed={selection[tag.name] === 'include'} onClick={() => toggle(tag.name, 'include')}>Include</button>
          <button className="secondary" aria-pressed={selection[tag.name] === 'exclude'} onClick={() => toggle(tag.name, 'exclude')}>Exclude</button></div></article>)}
    </div>}
  </section>
}

export function Collections({ generation, refresh, browse, field, fail }: {
  generation: number; refresh: () => void; browse: (item: LedgerObject) => void; field: (value: Field) => void; fail: Fail
}) {
  const [collections, setCollections] = useState<LedgerObject[]>([])
  const [edges, setEdges] = useState<Edge[]>([])
  const [container, setContainer] = useState('')
  useEffect(() => {
    let active = true
    Promise.all([ledger<LedgerObject[]>('search', { query: 'virtual:true', options: { limit: 500 } }), ledger<Edge[]>('graph', { reference: container || undefined })])
      .then(([collections, edges]) => { if (active) { setCollections(collections); setEdges(edges) } })
      .catch((error) => { if (active) fail(error) })
    return () => { active = false }
  }, [generation, container])
  const current = collections.find((item) => item.id === container)
  function create(marriage: boolean) {
    field({ title: marriage ? 'New Marriage' : 'New Coll', label: 'Name', apply: async (name) => {
      setContainer(await ledger<string>('collection', { name, marriage })); refresh()
    } })
  }
  return <section className="area">
    <div className="heading"><div><h1>Collections</h1><p>Browse virtual Colls and Marriages.</p></div><div className="inline">
      <button className="secondary" onClick={() => create(false)}><Plus size={16} /> Coll</button>
      <button className="primary" onClick={() => create(true)}><Plus size={16} /> Marriage</button></div></div>
    {collections.length === 0 ? <Empty icon={<Network size={28} />} title="No virtual collections" /> : <>
      <div className="toolbar"><label>Filter relationships <select value={container} onChange={(event) => setContainer(event.target.value)}>
        <option value="">All collections</option>{collections.map((item) => <option key={item.id} value={item.id}>{item.name}</option>)}</select></label>
        {current && <><button className="secondary" onClick={() => browse(current)}>Browse <ArrowUpRight size={15} /></button>
          <button className="secondary" onClick={() => field({ title: 'Add a relationship', label: 'Indexed path or Mosh ID', apply: async (reference) => {
            await ledger('connect', { reference, container, kind: current.kind === 'marriage' ? 'reference' : 'parent' }); refresh()
          } })}><Plus size={15} /> Add reference</button></>}
      </div>
      <div className="card"><h2>Relationships</h2>{edges.length ? <div className="graph-list">
        {edges.map((edge) => <div className="graph-edge" key={[edge.source.id, edge.target.id, edge.kind, edge.selector].join(':')}>
          <button onClick={() => setContainer(edge.source.id)}><ObjectIcon item={edge.source} />{edge.source.name}</button>
          <span>{edge.kind}<ArrowRight size={17} /></span>
          <button onClick={() => edge.target.kind !== 'file' ? browse(edge.target) : ledger('open', { reference: edge.target.id }).catch(fail)}><ObjectIcon item={edge.target} />{edge.target.name}</button></div>)}
      </div> : <p className="muted">No stored relationships match this view.</p>}</div></>}
  </section>
}

export function Disk({ roots, initial, task, busy, fail }: { roots: string[]; initial: string; task: Task; busy: boolean; fail: Fail }) {
  const [path, setPath] = useState(initial)
  const [space, setSpace] = useState<Space>()
  const [groups, setGroups] = useState<DuplicateGroup[] | null>(null)
  useEffect(() => {
    let active = true
    if (path) ledger<Space>('space', { path }).then((space) => {
      if (active) { setSpace(space); setGroups(null) }
    }).catch((error) => { if (active) fail(error) })
    return () => { active = false }
  }, [path])
  const max = Math.max(...(space?.tree.map((item) => item.logical) ?? []), 1)
  return <section className="area">
    <div className="heading"><div><h1>Disk usage</h1>{path && <p className="path">{path}</p>}</div>
      <button className="secondary" disabled={busy || !path} onClick={async () => {
        const result = await task('Verifying duplicate candidates…', () => ledger<{ groups: DuplicateGroup[] }>('duplicates', { path }))
        if (result) setGroups(result.groups)
      }}><Archive size={16} /> Find duplicates</button></div>
    <div className="toolbar"><label>Indexed folder <select value={path} onChange={(event) => setPath(event.target.value)}>
      {path && !roots.includes(path) && <option>{path}</option>}{!path && <option value="">Choose a folder</option>}{roots.map((root) => <option key={root}>{root}</option>)}</select></label></div>
    {space ? <>
      <div className="metrics"><div><span>Logical size</span><strong>{bytes(space.logical)}</strong></div>
        <div><span>Known allocation</span><strong>{bytes(space.known_allocated)}</strong>{space.unknown_allocation > 0 && <small>{space.unknown_allocation} unknown</small>}</div>
        <div><span>Files</span><strong>{space.files.toLocaleString()}</strong></div></div>
      <div className="card"><h2>Storage by location</h2>{space.tree.map((item) => <div className="bar-row" key={item.name}>
        <span title={item.name}>{item.name}</span><div className="bar-track"><div style={{ width: (item.logical / max * 100).toFixed(1) + '%' }} /></div><span>{bytes(item.logical)}</span>
      </div>)}</div>
      <div className="card"><h2>Size distribution</h2>{space.distribution.map((bin) => <div className="stats-row" key={bin.label}>
        <span>{bin.label}</span><span>{bin.files.toLocaleString()} files</span><span>{bytes(bin.logical)}</span></div>)}</div>
    </> : <Empty icon={<HardDrive size={28} />} title="Select an indexed folder" />}
    {groups && <div className="card"><h2>Exact duplicates</h2>{!groups.length && <p className="muted">No duplicate groups found.</p>}
      {groups.map((group) => <details key={group.hash}><summary>{group.members.length} paths · {group.physical_files} files · {bytes(group.size)} each</summary>
        <p>Estimated reclaimable allocation {bytes(group.reclaimable_allocated)}</p>
        {group.members.map((member) => <p className="path" key={member.id}>{member.path}</p>)}</details>)}</div>}
  </section>
}

export function Cleave({ initial, marked, refresh, task, busy }: { initial: string; marked: string[]; refresh: () => void; task: Task; busy: boolean }) {
  const [query, setQuery] = useState(initial)
  const [action, setAction] = useState<Action>('manifest')
  const [argument, setArgument] = useState('')
  const [manifest, setManifest] = useState<Manifest>()
  const [result, setResult] = useState<ResultPlan>()
  const [confirmed, setConfirmed] = useState(false)
  const physical = ['copy', 'move', 'quarantine'].includes(action)
  function clear() { setManifest(undefined); setResult(undefined); setConfirmed(false) }
  function download() {
    const url = URL.createObjectURL(new Blob([JSON.stringify(manifest, null, 2)], { type: 'application/json' }))
    const link = document.createElement('a'); link.href = url; link.download = 'mosh-manifest.json'; link.click()
    window.setTimeout(() => URL.revokeObjectURL(url), 500)
  }
  return <section className="area">
    <div className="heading"><div><h1>Cleave</h1><p>Preview a selection and its destinations before making changes.</p></div></div>
    <form className="card cleave-form" onSubmit={async (event) => {
      event.preventDefault(); clear()
      const plan = await task('Building the manifest…', () => ledger<Manifest>('plan', { query, action, selected: marked, argument: argument || undefined }))
      if (plan) setManifest(plan)
    }}>
      {marked.length ? <p>{marked.length} selected objects</p> : <label>Ledger query<input value={query} placeholder="ext:log AND modified:>30d" onChange={(event) => { setQuery(event.target.value); clear() }} /></label>}
      <label>Action<select value={action} onChange={(event) => { setAction(event.target.value as Action); setArgument(''); clear() }}>
        <option value="manifest">Export manifest</option><option value="tag">Tag selection</option><option value="marriage">Create Marriage</option>
        <option value="copy">Copy files</option><option value="move">Move files</option><option value="quarantine">Quarantine files</option></select></label>
      {['tag', 'marriage', 'copy', 'move'].includes(action) && <label>{action === 'tag' ? 'Tag' : action === 'marriage' ? 'Marriage name' : 'Destination folder'}
        <input required value={argument} onChange={(event) => { setArgument(event.target.value); clear() }} /></label>}
      <button className="primary" disabled={busy || (action !== 'manifest' && !query.trim() && !marked.length)}><Layers3 size={16} /> Preview</button>
    </form>
    {manifest && <div className="card">
      <div className="heading"><div><h2>Manifest</h2><p>{manifest.entries.length.toLocaleString()} objects · {bytes(manifest.total_bytes)} selected · {bytes(manifest.added_logical_bytes)} added</p></div>
        <button className="secondary" onClick={download}><ArrowDown size={16} /> Export JSON</button></div>
      {manifest.conflicts.map((item, index) => <p className="conflict" key={index}>{item}</p>)}
      {manifest.warnings.map((item, index) => <p className="warning" key={index}>{item}</p>)}
      <div className="manifest" aria-label="Affected objects">{manifest.entries.map((entry) => <div key={entry.object}><span>{entry.source}</span>
        {entry.destination && <small><ArrowRight size={13} /> {entry.destination}</small>}</div>)}</div>
      {physical && <label className="check confirm"><input type="checkbox" checked={confirmed} onChange={(event) => setConfirmed(event.target.checked)} /> Confirm the displayed file changes</label>}
      {!result && action !== 'manifest' && <button className="primary" disabled={busy || !!manifest.conflicts.length || (physical && !confirmed)} onClick={async () => {
        const value = await task('Executing Cleave…', () => ledger<ResultPlan>('execute', { id: manifest.id, confirmed }))
        if (value) { setResult(value); setConfirmed(false); refresh() }
      }}>Execute plan</button>}
      {result && <><p role="status">Plan {result.state} · {result.steps.length} recorded steps</p>
        {result.steps.filter((step) => step.details?.error).map((step) => <p className="conflict" key={step.step}>{step.details?.error}</p>)}
        {physical && ['done', 'partial', 'undo-partial'].includes(result.state) && <button className="secondary" disabled={busy || !confirmed} onClick={async () => {
          const value = await task('Restoring files…', () => ledger<ResultPlan>('undo', { id: manifest.id, confirmed }))
          if (value) { setResult(value); setConfirmed(false); refresh() }
        }}>Undo plan</button>}</>}
    </div>}
  </section>
}

export function History({ generation, fail }: { generation: number; fail: Fail }) {
  const [events, setEvents] = useState<Activity[]>([])
  const [filter, setFilter] = useState('')
  useEffect(() => { ledger<Activity[]>('history', { limit: 250 }).then(setEvents).catch(fail) }, [generation])
  const path = (event: Activity) => event.after?.path ?? event.before?.path ?? ''
  const action = (event: Activity) => event.action.replaceAll('-', ' ')
  const visible = events.filter((event) => (action(event) + ' ' + path(event)).toLowerCase().includes(filter.toLowerCase()))
  return <section className="area">
    <div className="heading"><div><h1>History</h1><p>Mosh-observed changes and operations.</p></div></div>
    <div className="toolbar"><input aria-label="Filter activity" placeholder="Filter recent activity" value={filter} onChange={(event) => setFilter(event.target.value)} /></div>
    {!visible.length ? <Empty icon={<Clock3 size={28} />} title="No matching activity" /> : <div className="activity">
      {visible.map((event, index) => <article key={event.time + ':' + index}><Clock3 size={17} /><div><strong>{action(event)}</strong><p className="path">{path(event)}</p></div>
        <span>{event.actor ?? 'Unknown actor'}</span><time>{displayDate(event.time)}</time></article>)}</div>}
  </section>
}
