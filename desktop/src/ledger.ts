import { invoke } from '@tauri-apps/api/core'

export interface LedgerObject {
  id: string
  kind: 'file' | 'coll' | 'marriage'
  path: string | null
  parent: string | null
  name: string
  extension: string | null
  mime: string | null
  size: number
  allocated: number | null
  created: number | null
  modified: number | null
  accessed: number | null
  changed: number | null
  attributes: number | null
  native_id: string | null
  volume: string | null
  filesystem: string | null
  hardlinks: number | null
  link_target: string | null
  reparse: boolean
  hash: string | null
  revision: number
  git_root: string | null
  git_status: string | null
  git_author: string | null
  git_committed: number | null
}

export interface Tag {
  name: string
  origin: string
  confidence: number
  inherited: boolean
  evidence: string | null
}

export interface Paint {
  level: string
  body: string
  stale: boolean
  locked: boolean
}

export interface Details {
  object: LedgerObject
  tags: Tag[]
  paint: Paint | null
  owners: string[]
  windows_security: { sddl?: string } | null
}

export interface TagCount { name: string; count: number }
export interface Saved { name: string; query: string }
export interface TreeBranch { name: string; files: number; logical: number }
export interface Space {
  root: string
  files: number
  logical: number
  known_allocated: number
  unknown_allocation: number
  largest: LedgerObject[]
  tree: TreeBranch[]
  distribution: { label: string; files: number; logical: number }[]
}
export interface DuplicateGroup {
  hash: string
  size: number
  members: LedgerObject[]
  physical_files: number
  reclaimable_allocated: number | null
}
export interface Edge {
  source: LedgerObject
  target: LedgerObject
  kind: string
  selector: string
}
export interface Activity {
  time: number
  object: string | null
  action: string
  actor: string | null
  before: { path?: string } | null
  after: { path?: string } | null
  outcome: string
}
export interface Commit { commit: string; authored: string; committed: string; author: string; committer: string; subject: string }
export type Action = 'manifest' | 'tag' | 'marriage' | 'copy' | 'move' | 'quarantine'
export interface Manifest {
  id: string
  action: Action
  total_bytes: number
  added_logical_bytes: number
  conflicts: string[]
  warnings: string[]
  entries: { object: string; source: string; destination: string | null; bytes: number }[]
}
export interface ResultPlan {
  state: string
  steps: { step: number; state: string; details?: { error?: string } }[]
}

export function ledger<T>(name: string, payload: object = {}): Promise<T> {
  return invoke<T>('ledger', { name, payload })
}

export function bytes(size: number | null): string {
  if (size === null) return 'Unavailable'
  if (size < 1024) return size.toLocaleString() + ' B'
  const units = ['KiB', 'MiB', 'GiB', 'TiB', 'PiB']
  let value = size / 1024
  let i = 0
  while (value >= 1024 && i < units.length - 1) { value /= 1024; i++ }
  return value.toLocaleString(undefined, { maximumFractionDigits: 1 }) + ' ' + units[i]
}

export function displayDate(value: number | null): string {
  return value === null ? 'Unavailable' : new Intl.DateTimeFormat(undefined, {
    year: 'numeric', month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit',
  }).format(value)
}

export function leaf(path: string): string {
  return path.split(/[\\/]+/).filter(Boolean).at(-1) ?? path
}

export function quoted(value: string): string {
  return '"' + value.replaceAll('"', '\\"') + '"'
}
