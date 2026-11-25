import { useEffect, useState } from 'react'
import { ledger, type LedgerObject } from './ledger'

type Fail = (error: unknown) => void
type TierRule = { level: number; minimum_tags: number; minimum_score: number; passes: number; paint: string | null; embed: boolean }
type TierSettings = { tiers: TierRule[]; weights: { importance: number; information: number; hotness: number; promote: boolean; demote: boolean; hysteresis: number } }
type Config = Record<string, string | boolean | number | string[] | null>
type Finding = { object: LedgerObject; why: string[]; excerpt: string | null; certainty: string; citation: { id: number } | null }
type SearchRun = { id: number | null; results: Finding[]; answer: string | null; metrics: { total_ms: number }; notes: string[];
  child_suggestions: { child: string; name: string; paint: string }[] }

const textSettings: Record<string, string> = {
  endpoint: 'Endpoint', classification: 'Classification model', embedding: 'Embedding model', reasoning: 'Reasoning model',
  planning: 'Planning model', fallback: 'Fallback model', self_name: 'Name used for “me”',
}
const numberSettings: Record<string, string> = {
  context_bytes: 'Source context bytes', output_tokens: 'Output token limit', timeout_seconds: 'Request timeout (seconds)',
  concurrency: 'Concurrent agent jobs', retries: 'Retries', latency_ms: 'Search latency goal (ms)', max_candidates: 'Candidate limit',
  pass_hits: 'Confirmations before promoting a fragment', idle_seconds: 'Idle time before background work (seconds)',
  minimum_free_ram_mb: 'Free RAM required (MiB)', daily_micros: 'Daily budget (USD millionths)',
  monthly_micros: 'Monthly budget (USD millionths)', input_micros_per_million: 'Input rate (USD millionths per million tokens)',
  output_micros_per_million: 'Output rate (USD millionths per million tokens)',
}
const flags = { allow_remote: 'Allow remote inference', learn: 'Record retrievals and learn from feedback', semantic: 'Use semantic indexes', background: 'Run agents when idle' }

export function Settings({ fail }: { fail: Fail }) {
  const [tiers, setTiers] = useState<TierSettings | null>(null)
  const [config, setConfig] = useState<Config | null>(null)
  const [status, setStatus] = useState<unknown>(null)
  const [saved, setSaved] = useState('')
  useEffect(() => {
    ledger<TierSettings>('tier_settings').then(setTiers).catch(fail)
    ledger<Config>('ai_settings').then(setConfig).catch(fail)
  }, [])
  const update = (name: string, value: Config[string]) => setConfig(current => current && ({ ...current, [name]: value }))
  const rule = (level: number, values: Partial<TierRule>) => setTiers(current => current && ({
    ...current, tiers: current.tiers.map(tier => tier.level === level ? { ...tier, ...values } : tier),
  }))
  return <section className="panel intel"><h2>Settings</h2>
    {tiers && <form onSubmit={event => {
      event.preventDefault(); ledger<TierSettings>('configure_tiers', tiers).then(result => { setTiers(result); setSaved('Tiers saved') }).catch(fail)
    }}>
      <h3>Information tiers</h3><p className="muted">A file earns a tier when it has at least the required number of distinct confirmed direct tags.</p>
      <div className="intel-grid">
        {(['importance', 'information', 'hotness', 'hysteresis'] as const).map(key => <label key={key}>{key}
          <input type="number" min="0" value={tiers.weights[key]} onChange={event => setTiers({ ...tiers, weights: { ...tiers.weights, [key]: Number(event.target.value) } })} />
        </label>)}
        {(['promote', 'demote'] as const).map(key => <label className="check" key={key}>
          <input type="checkbox" checked={tiers.weights[key]} onChange={event => setTiers({ ...tiers, weights: { ...tiers.weights, [key]: event.target.checked } })} /> Automatic {key === 'promote' ? 'promotion' : 'demotion'}
        </label>)}
      </div>
      {tiers.tiers.map(tier => <fieldset className="intel-grid" key={tier.level}><legend>T{tier.level}</legend>
        <label>Minimum tags<input type="number" min="1" value={tier.minimum_tags} onChange={event => rule(tier.level, { minimum_tags: Number(event.target.value) })} /></label>
        <label>Score threshold<input type="number" min="0" max="100" value={tier.minimum_score} onChange={event => rule(tier.level, { minimum_score: Number(event.target.value) })} /></label>
        <label>Analysis passes<input type="number" min="1" max="8" value={tier.passes} onChange={event => rule(tier.level, { passes: Number(event.target.value) })} /></label>
        <label>Paint<select value={tier.paint ?? ''} onChange={event => rule(tier.level, { paint: event.target.value || null })}>
          {['', 'minimal', 'standard', 'detailed'].map(level => <option key={level} value={level}>{level || 'None'}</option>)}</select></label>
        <label className="check"><input type="checkbox" checked={tier.embed} onChange={event => rule(tier.level, { embed: event.target.checked })} /> Embed</label>
        {tier.level > 4 && tier.level === tiers.tiers.length && <button className="secondary" type="button" onClick={() => setTiers({ ...tiers, tiers: tiers.tiers.slice(0, -1) })}>Disable T{tier.level}</button>}
      </fieldset>)}
      <div className="inline">
        <button className="primary">Save tiers</button>
        {tiers.tiers.length < 10 && <button className="secondary" type="button" onClick={() => {
          const last = tiers.tiers[tiers.tiers.length - 1]
          setTiers({ ...tiers, tiers: [...tiers.tiers, { ...last, level: last.level + 1, minimum_tags: last.minimum_tags + 1, minimum_score: Math.min(last.minimum_score + 1, 100) }] })
        }}>Configure T{tiers.tiers.length + 1}</button>}
      </div>
    </form>}
    {config && <form onSubmit={event => {
      event.preventDefault(); ledger<Config>('configure_ai', config).then(result => { setConfig(result); setSaved('AI settings saved') }).catch(fail)
    }}><h3>AI providers and agents</h3><p className="muted">A local Ollama server can run the selected models. Remote keys come from the MOSH_AI_API_KEY or OPENAI_API_KEY environment variable.</p>
      <div className="intel-grid">
        <label>Provider<select value={String(config.provider)} onChange={event => {
          const provider = event.target.value
          setConfig({ ...config, provider, endpoint: provider === 'openai' ? 'https://api.openai.com/v1' :
            provider === 'ollama' ? 'http://127.0.0.1:11434' : '' })
        }}>
          <option value="ollama">Ollama</option><option value="openai">OpenAI</option><option value="compatible">OpenAI-compatible</option>
        </select></label>
        <label>Agent intensity<select value={String(config.mode)} onChange={event => update('mode', event.target.value)}>
          {['off', 'light', 'normal', 'deep', 'exhaustive', 'custom'].map(mode => <option key={mode}>{mode}</option>)}
        </select></label>
        <label>Search mode<select value={String(config.search_mode)} onChange={event => update('search_mode', event.target.value)}>
          {['fast', 'balanced', 'exhaustive'].map(mode => <option key={mode}>{mode}</option>)}
        </select></label>
        <label>Reasoning effort<select value={String(config.reasoning_effort ?? '')} onChange={event => update('reasoning_effort', event.target.value || null)}>
          <option value="">Provider default</option>{['low', 'medium', 'high'].map(level => <option key={level}>{level}</option>)}
        </select></label>
        {Object.entries(textSettings).map(([key, label]) => <label key={key}>{label}
          <input value={String(config[key] ?? '')} onChange={event => update(key, event.target.value)} /></label>)}
        {Object.entries(numberSettings).map(([key, label]) => <label key={key}>{label}
          <input type="number" min="0" value={Number(config[key])} onChange={event => update(key, Number(event.target.value))} /></label>)}
        {Object.entries(flags).map(([key, label]) => <label className="check" key={key}><input type="checkbox" checked={Boolean(config[key])} onChange={event => update(key, event.target.checked)} /> {label}</label>)}
        <label>Paths excluded from remote AI<textarea value={(config.remote_excluded_paths as string[]).join('\n')} onChange={event => update('remote_excluded_paths', event.target.value.split('\n'))} /></label>
      </div>
      <div className="inline"><button className="primary">Save AI settings</button>
        <button type="button" className="secondary" onClick={() => ledger('model_status').then(setStatus).catch(fail)}>Check models</button>
        {config.provider === 'ollama' && <button type="button" className="secondary" onClick={() => {
          ledger<Config>('configure_detected_models').then(result => { setConfig(result); setSaved('Installed models selected') }).catch(fail)
        }}>Use detected models</button>}
        <a href="https://ollama.com/download" target="_blank" rel="noreferrer">Ollama setup</a></div>
      {status != null && <pre>{JSON.stringify(status, null, 2)}</pre>}
    </form>}
    {saved && <p role="status">{saved}</p>}
  </section>
}

export function Ask({ fail, reveal, route }: { fail: Fail; reveal: (item: LedgerObject) => void; route: (child: string) => void }) {
  const [question, setQuestion] = useState('')
  const [answer, setAnswer] = useState(true)
  const [result, setResult] = useState<SearchRun | null>(null)
  const [busy, setBusy] = useState(false)
  const [feedback, setFeedback] = useState('')
  const record = (reference: string, helpful: boolean, passage?: number) => {
    ledger('feedback', { run: result?.id, reference, helpful, passage }).then(() => setFeedback('Feedback recorded')).catch(fail)
  }
  return <section className="panel intel"><h2>Ask Mosh</h2><form onSubmit={event => {
    event.preventDefault(); setFeedback(''); setBusy(true)
    ledger<SearchRun>('ask', { query: question, answer }).then(setResult).catch(fail).finally(() => setBusy(false))
  }}><label>What are you looking for?<textarea required maxLength={2048} value={question} onChange={event => setQuestion(event.target.value)} /></label>
    <div className="inline"><button className="primary" disabled={busy}>{busy ? 'Searching…' : 'Search'}</button>
      <label className="check"><input type="checkbox" checked={answer} onChange={event => setAnswer(event.target.checked)} /> Include excerpts</label></div>
  </form>
    {result && <><p>{result.results.length.toLocaleString()} results in {result.metrics.total_ms.toLocaleString()} ms</p>
      {result.answer && <blockquote>{result.answer}</blockquote>}
      {result.notes.map(note => <p className="muted" key={note}>{note}</p>)}
      {result.child_suggestions.map(pointer => <p key={pointer.child}>
        <button className="secondary" onClick={() => route(pointer.child)}>Inspect child {pointer.name}</button> {pointer.paint}
      </p>)}
      {result.results.map(item => <article className="inspect-block" key={item.object.id}>
        <button className="secondary" onClick={() => reveal(item.object)}>{item.object.name}</button>
        <p className="muted">{item.certainty === 'verified-excerpt' ? 'Verified source excerpt' : item.certainty === 'approximate' ? 'Approximate match' : 'Candidate'} · {item.why.join(' · ')}</p>
        {item.excerpt && <blockquote>{item.excerpt}</blockquote>}
        {result.id !== null && <div className="inline">
          <button className="secondary" onClick={() => record(item.object.id, true, item.citation?.id)}>Helpful</button>
          <button className="secondary" onClick={() => record(item.object.id, false)}>Irrelevant</button></div>}
      </article>)}
      {result.id !== null && <form className="inline" onSubmit={event => {
        event.preventDefault(); const path = new FormData(event.currentTarget).get('source'); if (typeof path === 'string') record(path, true)
      }}><input name="source" required placeholder="Path to a correct missing result" aria-label="Path to a correct missing result" /><button className="secondary">Confirm missing result</button></form>}
      {feedback && <p role="status">{feedback}</p>}
    </>}
  </section>
}

export function Agents({ fail }: { fail: Fail }) {
  const [info, setInfo] = useState<unknown>(null)
  const [query, setQuery] = useState('')
  const [busy, setBusy] = useState(false)
  const run = (name: string, params: object = {}) => {
    setBusy(true); ledger(name, params).then(setInfo).catch(fail).finally(() => setBusy(false))
  }
  useEffect(() => run('ai_queue'), [])
  return <section className="panel intel"><h2>Agents</h2>
    <form className="inline" onSubmit={event => { event.preventDefault(); run('queue_index', { query, limit: 50 }) }}>
      <input aria-label="Ledger query to index" placeholder="Ledger query, such as ext:txt" value={query} onChange={event => setQuery(event.target.value)} />
      <button className="primary" disabled={busy}>Queue 50 objects</button>
    </form>
    <div className="inline">{Object.entries({ ai_queue: 'Queue', ai_run: 'Run next 10 jobs', ai_usage: 'Model usage', advisor: 'System Advisor' }).map(([name, label]) =>
      <button className="secondary" key={name} disabled={busy} onClick={() => run(name, { limit: name === 'ai_run' ? 10 : 50 })}>{label}</button>)}</div>
    {busy && <p role="status">Working…</p>}{info != null && <pre>{JSON.stringify(info, null, 2)}</pre>}
    <form onSubmit={event => {
      event.preventDefault()
      const form = new FormData(event.currentTarget)
      run('ai_policy', { reference: form.get('coll'), mode: form.get('mode') || null, allow_remote: form.has('remote') })
    }}><h3>Coll processing policy</h3><div className="intel-grid">
      <label>Coll path or ID<input name="coll" required /></label>
      <label>Intensity<select name="mode"><option value="">Inherit</option>
        {['off', 'light', 'normal', 'deep', 'exhaustive', 'custom'].map(mode => <option key={mode}>{mode}</option>)}
      </select></label>
      <label className="check"><input type="checkbox" name="remote" /> Allow this Coll’s content to be sent to the configured remote provider</label>
      </div><button className="secondary" disabled={busy}>Save Coll policy</button></form>
    <form onSubmit={event => {
      event.preventDefault()
      const form = new FormData(event.currentTarget)
      run('coll_security', { reference: form.get('coll'), restricted: form.has('restricted'), person: form.get('person') || null, rights: form.getAll('rights') })
    }}><h3>Coll permissions</h3><div className="intel-grid">
      <label>Coll path or ID<input name="coll" required /></label>
      <label>Existing guest account<input name="person" /></label>
      <label className="check"><input type="checkbox" name="restricted" defaultChecked /> Restrict access</label>
      {['discover', 'paint', 'content', 'edit', 'manage'].map(right => <label className="check" key={right}><input type="checkbox" name="rights" value={right} /> {right}</label>)}
      </div><button className="secondary" disabled={busy}>Save permissions</button></form>
  </section>
}

export function TagRegistry({ fail }: { fail: Fail }) {
  const [tag, setTag] = useState('')
  const [into, setInto] = useState('')
  const [query, setQuery] = useState('')
  const [category, setCategory] = useState('')
  const [description, setDescription] = useState('')
  const [result, setResult] = useState<unknown>(null)
  const [reviewed, setReviewed] = useState(false)
  const run = (name: string, params: object = {}) => {
    setReviewed(false)
    ledger(name, { tag, into, query, limit: 50, ...params }).then(setResult).catch(fail)
  }
  return <details className="panel intel"><summary>Tag Registry</summary>
    <div className="intel-grid">
      <label>Tag<input value={tag} onChange={event => { setTag(event.target.value); setReviewed(false) }} /></label>
      <label>Category<input value={category} onChange={event => setCategory(event.target.value)} /></label>
      <label>Description<input value={description} onChange={event => setDescription(event.target.value)} /></label>
      <label>Replacement, alias or new name<input value={into} onChange={event => { setInto(event.target.value); setReviewed(false) }} /></label>
      <label>Ledger query for backfill or split<input value={query} onChange={event => { setQuery(event.target.value); setReviewed(false) }} /></label>
    </div>
    <div className="inline">
      <button className="secondary" onClick={() => run('tag_registry', { tag: tag || null })}>Registry entries</button>
      {Object.entries({ tag_history: 'History', tag_preview: 'Affected objects', queue_backfill: 'Queue backfill' }).map(([name, label]) =>
        <button className="secondary" key={name} disabled={!tag.trim()} onClick={() => run(name)}>{label}</button>)}
    </div>
    <div className="inline">
      {['create', 'describe', 'retire', 'reactivate'].map(action =>
        <button className="secondary" key={action} disabled={!tag.trim()} onClick={() => run('maintain_tag', { action, category: category || null, description: description || null, replacement: into || null })}>{action}</button>)}
      {['alias', 'rename', 'merge'].map(action =>
        <button className="secondary" key={action} disabled={!tag.trim() || !into.trim()} onClick={() => {
          if (action !== 'merge' || window.confirm(`Merge every assignment of ${tag} into ${into}?`)) run('tag_dictionary', { action, confirmed: action === 'merge' })
        }}>{action}</button>)}
      <button className="secondary" disabled={!tag.trim() || !into.trim() || !query.trim()} onClick={() => {
        ledger('tag_split', { tag, into, query, confirmed: false }).then(preview => { setResult(preview); setReviewed(true) }).catch(fail)
      }}>Preview split</button>
      {reviewed && <button className="primary" onClick={() => run('tag_split', { confirmed: true })}>Confirm split</button>}
    </div>
    {result != null && <pre>{JSON.stringify(result, null, 2)}</pre>}
  </details>
}
