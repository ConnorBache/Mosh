import { useEffect, useState } from 'react'
import { ledger, type LedgerObject } from './ledger'

type Pointer = { child: string; name: string; paint: string; tags: string[]; root?: string }
type ScopeInfo = { mosh: string; children: Pointer[] }
type Handoff = { results: { object: LedgerObject; qualified_id: string; excerpt?: string }[] }

export function Scopes({ fail, refresh, initialChild }: { fail: (error: unknown) => void; refresh: () => void; initialChild: string }) {
  const [info, setInfo] = useState<ScopeInfo | null>(null)
  const [child, setChild] = useState(initialChild)
  const [result, setResult] = useState<Handoff | null>(null)
  const [status, setStatus] = useState('')
  const load = () => ledger<ScopeInfo>('mosh_info').then(setInfo).catch(fail)
  useEffect(() => { void load() }, [])
  const selected = info?.children.find(pointer => pointer.child === child)
  const change = (command: string, payload: object, message: string) =>
    ledger(command, payload).then(() => { setStatus(message); refresh(); return load() }).catch(fail)

  return <section className="panel intel"><h2>Nested Moshes</h2>
    {info && <p>Current Mosh ID <code>{info.mosh}</code></p>}
    <div className="inline">{info?.children.map(pointer =>
      <button className="secondary" key={pointer.child} type="button" aria-pressed={child === pointer.child}
        onClick={() => { setChild(pointer.child); setResult(null) }}>{pointer.name}</button>)}</div>
    {selected && <article><h3>{selected.name}</h3><p>{selected.paint}</p><p>{selected.tags.join(' · ')}</p>
      <form onSubmit={event => {
        event.preventDefault()
        const form = new FormData(event.currentTarget)
        ledger<Handoff>('mosh_handoff', { child, query: form.get('query'), natural: form.has('natural') }).then(setResult).catch(fail)
      }}>
        <label>Search this child<input name="query" required /></label>
        <label className="check"><input type="checkbox" name="natural" /> Interpret as a question</label>
        <button className="primary">Search child</button>
      </form>
      {result && (result.results.length ? <ul>{result.results.map(hit => <li key={hit.qualified_id}>
        <strong>{hit.object.name}</strong><p>{hit.excerpt ?? hit.qualified_id}</p>
      </li>)}</ul> : <p>No matches in this child.</p>)}
      <button className="secondary" onClick={() => {
        if (window.confirm(`Detach ${selected.name}?`)) void change('mosh_detach', { child }, 'Child detached').then(() => setChild(''))
      }}>Detach child</button>
    </article>}
    <form onSubmit={event => {
      event.preventDefault()
      const fields = new FormData(event.currentTarget)
      const text = (name: string) => String(fields.get(name) ?? '').trim()
      void change('mosh_attach', { path: text('path'), root: text('root'), name: text('name'),
        paint: text('paint'), tags: text('tags').split(',').map(tag => tag.trim()).filter(Boolean) }, 'Child attached')
    }}><h3>Attach a child</h3>
      <div className="intel-grid">
        <label>Child Ledger path<input name="path" required /></label>
        <label>Child root directory<input name="root" required /></label>
        <label>Display name<input name="name" required /></label>
        <label>Routing tags<input name="tags" placeholder="Comma-separated tags" /></label>
        <label>Paint<textarea name="paint" /></label>
      </div><button className="secondary">Attach child</button>
    </form>
    {selected && <form onSubmit={event => {
      event.preventDefault()
      const form = new FormData(event.currentTarget)
      void change('mosh_discovery', { child, actor: form.get('actor'), enabled: form.has('enabled') }, 'Discovery policy saved')
    }}><h3>Child discovery</h3><label>Existing actor<input name="actor" required /></label>
      <label className="check"><input name="enabled" type="checkbox" defaultChecked /> Allow discovery</label>
      <button className="secondary">Save discovery policy</button>
    </form>}
    <form onSubmit={event => {
      event.preventDefault()
      const form = new FormData(event.currentTarget)
      void change('mosh_allow', { parent: form.get('parent'), actor: form.get('actor'), enabled: form.has('enabled') }, 'Search policy saved')
    }}><h3>Search permission for this Mosh</h3><div className="intel-grid">
      <label>Parent Mosh ID<input name="parent" required /></label><label>Existing actor<input name="actor" defaultValue="local" required /></label>
      <label className="check"><input name="enabled" type="checkbox" defaultChecked /> Allow explicit handoff</label>
      </div><button className="secondary">Save search policy</button>
    </form>
    {status && <p role="status">{status}</p>}
  </section>
}
