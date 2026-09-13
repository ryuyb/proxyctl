/**
 * The wire types, mirroring `crates/interfaces/src/dto/mod.rs`.
 *
 * # Why these are written by hand
 *
 * See the note in `api.ts`. The short version: the Rust DTOs are mapped field by
 * field *on purpose*, so that a change to the application's model cannot silently
 * become a change to the API. A generated client would undo that, so these are
 * written out and their names match the Rust fields exactly.
 *
 * # Why `snake_case`
 *
 * The agent serialises without a rename attribute, so the wire is `snake_case`
 * and these follow it. Renaming to camelCase here would add a mapping layer whose
 * only purpose would be to hide the actual field names from whoever is reading a
 * network trace.
 *
 * # Nullability
 *
 * `Option<T>` on the Rust side is `T | null` here, not `T | undefined`. The agent
 * always writes the key and writes `null` when it has nothing, and conflating the
 * two would make `value === undefined` and `value === null` both look like
 * "absent" when only one of them is what the server sends.
 */

/** One of the five capability states. Never collapsed to a boolean. */
export type CapabilityStatus =
  | 'supported'
  | 'unsupported'
  | 'unavailable'
  | 'misconfigured'
  | 'unknown'

/** How serious a doctor finding is. */
export type Severity = 'pass' | 'info' | 'warning' | 'error'


/** The kernel's lifecycle summary. */
export interface Status {
  status: string
  live: boolean
  serving: boolean
}

/** The running kernel build. */
export interface Build {
  version: string
  flavor: string
}

/** The lazy health answer. */
export interface Health {
  process_alive: boolean
  controller_reachable: boolean
  config_loaded: boolean
  proxy_port_listening: boolean
  healthy: boolean
  degraded: boolean
  summary: string
}

/** The kernel's lifecycle state. */
export interface MihomoStatus {
  instance: string
  name: string
  state: Status
  active_config: string | null
  build: Build | null
  health: Health | null
  last_failure: string | null
}

/** A configuration version. */
export interface ConfigVersion {
  id: string
  label: string
  source: string
  checksum: string
  active: boolean
  created_at: number
  activated_at: number | null
}

/** A subscription. */
export interface Subscription {
  id: string
  name: string
  enabled: boolean
  interval_seconds: number | null
  is_due: boolean
  last_update: string | null
}

/** A job. */
export interface Job {
  id: string
  kind: string
  target: string
  state: string
  step: string | null
  detail: string | null
  degradation: string | null
  created_at: number
  updated_at: number
}

/** An audit record. */
export interface Audit {
  id: string
  action: string
  actor: string
  target: string
  succeeded: boolean
  reason: string | null
  at: number
}

/** One environment field. */
export interface Environment {
  os: string
  os_version: string | null
  arch: string
  kernel: string | null
  init: string
  container: string
}

/** A capability observation. */
export interface Capability {
  kind: string
  status: CapabilityStatus
  evidence: string
}

/** The detected environment and its capabilities. */
export interface Capabilities {
  environment: Environment
  capabilities: Capability[]
}

/** A doctor finding. */
export interface Finding {
  severity: Severity
  code: string
  message: string
}

/** The doctor report. */
export interface Doctor {
  verdict: string
  findings: Finding[]
  environment: Environment
}

/** A strategy group. */
export interface ProxyGroup {
  name: string
  kind: string
  now: string | null
  members: string[]
}

/** A proxy node. */
export interface Proxy {
  name: string
  kind: string
  delay_millis: number | null
}

/** The kernel's proxy groups and nodes. */
export interface Proxies {
  groups: ProxyGroup[]
  proxies: Proxy[]
}

/** One live connection. */
export interface Connection {
  id: string
  source: string
  destination: string
  rule: string | null
  rule_payload: string | null
  chains: string[]
  /**
   * Administrative callers only.
   *
   * `null` means "not disclosed" rather than "not readable", and the server does
   * not distinguish the two — telling a caller why a field is empty would reveal
   * that it is populated for someone else. Render nothing rather than a dash for
   * these on a read-only session; a dash implies the agent looked and found
   * nothing.
   */
  uid: number | null
  process: string | null
  process_path: string | null
  started_at: number | null
  upload: number
  download: number
  inbound: string | null
}

/** The connection list. */
export interface Connections {
  upload_total: number
  download_total: number
  connections: Connection[]
}

/** The result of a close request. */
export interface CloseResult {
  /** `accepted`, or `rejected:<status>`. Never claim more than the kernel said. */
  outcome: string
  closed: number
  degradation: string | null
}

/** Who is signed in. */
export interface Session {
  principal: string
}

/** A created or updated resource's identifier. */
export interface Id {
  id: string
}

/** The result of validating a document. */
export interface Validation {
  preflight: string
  syntax: string
  semantic: string
  acceptable: boolean
}

/** A request to create or replace a subscription. */
export interface SubscriptionInput {
  name: string
  url: string
  user_agent?: string | null
  schedule_seconds?: number | null
}

/** A request to validate a configuration document. */
export interface ValidateInput {
  body: string
}

/**
 * One log line, as `/logs` returns it.
 *
 * Only two fields, because that is all the agent sends. There is deliberately no
 * `at`: the kernel's own log lines carry a `HH:MM:SS` prefix in their text and
 * nothing more, and inventing a date here would make the interface show a
 * timestamp the agent never observed — during a replay or a buffered read it
 * would be flatly wrong.
 */
export interface LogEntry {
  level: string
  message: string
}

/** One event from the stream. */
export interface AgentEvent {
  seq: number
  kind: string
  at: number
  data: Record<string, unknown>
}
