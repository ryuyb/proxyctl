/**
 * The English strings, and the canonical key set.
 *
 * This object is the source of truth for which keys exist; `zh.ts` is typed
 * against it, so a key added here is a type error until it is translated.
 */

export const en = {
  app: {
    name: 'Proxy Control',
    tagline: 'Mihomo management agent',
  },

  nav: {
    overview: 'Overview',
    mihomo: 'Kernel',
    configs: 'Configurations',
    subscriptions: 'Subscriptions',
    connections: 'Connections',
    logs: 'Logs',
    system: 'System',
    doctor: 'Doctor',
    signOut: 'Sign out',
  },

  language: {
    label: 'Language',
    en: 'English',
    zh: '中文',
  },

  roles: {
    admin: 'Administrator',
    'read-only': 'Read-only',
  },

  signIn: {
    title: 'Sign in',
    subtitle: 'Enter an API token issued by the agent.',
    tokenLabel: 'API token',
    tokenPlaceholder: 'Paste the token',
    submit: 'Sign in',
    submitting: 'Signing in…',
    failed: 'Sign-in failed',
    hint: 'Issue one with `proxyctl token issue` on the agent host.',
    // Stated here rather than left to be discovered from a refusal: the token is
    // exchanged for a cookie and is not kept.
    privacy: 'The token is exchanged for a session cookie and is not stored by this page.',
  },

  common: {
    retry: 'Retry',
    refresh: 'Refresh',
    cancel: 'Cancel',
    close: 'Close',
    confirm: 'Confirm',
    copy: 'Copy',
    copied: 'Copied',
    loading: 'Loading…',
    empty: 'Nothing to show',
    unknown: 'Unknown',
    none: 'None',
    never: 'Never',
    yes: 'Yes',
    no: 'No',
    error: 'Something went wrong',
    unreachable: 'The agent could not be reached',
    unreachableHint:
      'Check that the agent is running and that this page is talking to the right address.',
    forbidden: 'Your session is not permitted to do this',
    notFound: 'Not found',
    confirmTitle: 'Are you sure?',
    irreversible: 'This cannot be undone.',
    language: 'Language',
  },

  status: {
    // Lifecycle labels, from the kernel's own state machine.
    Running: 'Running',
    Stopped: 'Stopped',
    Starting: 'Starting',
    Stopping: 'Stopping',
    Failed: 'Failed',
    Degraded: 'Degraded',
    Unknown: 'Unknown',
    healthy: 'Healthy',
    unhealthy: 'Unhealthy',
    live: 'Live',
    serving: 'Serving',
    notServing: 'Not serving',
  },

  // The five capability states, never collapsed to a boolean. A reader has to be
  // able to tell "this kernel cannot do it" from "this container was not allowed
  // to" from "we could not find out", because the three call for different
  // responses.
  capability: {
    supported: 'Supported',
    unsupported: 'Unsupported',
    unavailable: 'Unavailable',
    misconfigured: 'Misconfigured',
    unknown: 'Unknown',
    probe: 'Probe',
  },

  severity: {
    pass: 'Pass',
    info: 'Info',
    warning: 'Warning',
    error: 'Error',
  },

  overview: {
    title: 'Overview',
    kernel: 'Kernel',
    activeConfig: 'Active configuration',
    version: 'Version',
    health: 'Health',
    noHealth: 'No health observation has been taken',
    runDoctor: 'Run a doctor check from the Doctor page to see the environment.',
    capabilities: 'Capabilities',
    recentJobs: 'Recent jobs',
    lastFailure: 'Last failure',
  },

  mihomo: {
    title: 'Kernel',
    lifecycle: 'Lifecycle',
    start: 'Start',
    stop: 'Stop',
    restart: 'Restart',
    reload: 'Reload',
    proxyGroups: 'Proxy groups',
    nodes: 'Nodes',
    groupNow: 'Selected',
    groupKind: 'Type',
    nodeKind: 'Protocol',
    nodeDelay: 'Delay',
    noDelay: 'Not measured',
    memberCount: '{{count}} members',
    delayValue: '{{ms}} ms',
    kernelVersion: 'Kernel',
    install: 'Install kernel',
    updating: 'Updating…',
    confirmRestart: 'Restarting briefly interrupts every connection. Continue?',
    confirmStop: 'Stopping takes the proxy down for every client. Continue?',
  },

  configs: {
    title: 'Configurations',
    label: 'Version',
    source: 'Source',
    checksum: 'Checksum',
    createdAt: 'Created',
    activatedAt: 'Activated',
    active: 'Active',
    activate: 'Activate',
    rollback: 'Roll back',
    validate: 'Validate',
    validateTitle: 'Validate a document',
    validateBody: 'Configuration body',
    validatePlaceholder: 'Paste a YAML configuration…',
    preflight: 'Preflight',
    syntax: 'Syntax',
    semantic: 'Semantic',
    acceptable: 'Acceptable',
    confirmActivate: 'Activating reloads the kernel with this version. Continue?',
    confirmRollback: 'Rolling back activates {{label}} again. Continue?',
    activated: 'Activated {{label}}',
    rolledBack: 'Rolled back to {{label}}',
  },

  subscriptions: {
    title: 'Subscriptions',
    name: 'Name',
    enabled: 'Enabled',
    interval: 'Interval',
    intervalNone: 'Not scheduled',
    intervalValue: '{{seconds}}s',
    due: 'Update due',
    lastUpdate: 'Last update',
    add: 'Add subscription',
    addTitle: 'New subscription',
    edit: 'Edit',
    url: 'Subscription URL',
    urlPlaceholder: 'https://example.com/sub?token=…',
    urlHint: 'Credentials in this URL are stored on the agent and are never shown again.',
    userAgent: 'User agent',
    schedule: 'Schedule',
    scheduleNone: 'Do not schedule',
    updateNow: 'Update now',
    remove: 'Delete',
    confirmRemove: 'Deleting a subscription cannot be undone. Continue?',
    created: 'Subscription created',
    updated: 'Subscription updated',
    removed: 'Subscription deleted',
  },

  connections: {
    title: 'Connections',
    source: 'Source',
    destination: 'Destination',
    rule: 'Rule',
    rulePayload: 'Rule payload',
    chains: 'Chain',
    process: 'Process',
    uid: 'UID',
    processPath: 'Executable',
    startedAt: 'Started',
    upload: 'Upload',
    download: 'Download',
    inbound: 'Inbound',
    close: 'Close',
    closeAll: 'Close all',
    confirmClose: 'Closing this connection interrupts its transfer. Continue?',
    confirmCloseAll:
      'This interrupts every active connection at once. This is the agent-wide kill switch.',
    totalUpload: 'Total upload',
    totalDownload: 'Total download',
    activeCount: '{{count}} active',
    // Stated for a read-only session, so an empty column is not read as "the
    // agent found nothing".
    processHidden: 'Process details are shown to administrators only.',
    closed: 'Closed {{count}} connection(s)',
    searchPlaceholder: 'Filter by host, process, or rule…',
  },

  logs: {
    title: 'Logs',
    level: 'Level',
    follow: 'Following',
    paused: 'Paused',
    pause: 'Pause',
    resume: 'Resume',
    clear: 'Clear',
    filterPlaceholder: 'Filter lines…',
    waiting: 'Waiting for the kernel to say something…',
    hiddenFromReadOnly: 'Kernel logs are not sent to a read-only session.',
    lines: '{{count}} lines',
    disconnected: 'The log stream closed. Reconnecting…',
  },

  system: {
    title: 'System',
    environment: 'Environment',
    os: 'Operating system',
    osVersion: 'Version',
    arch: 'Architecture',
    kernel: 'Kernel release',
    init: 'Init system',
    container: 'Container',
    capabilities: 'Capabilities',
    audit: 'Audit trail',
    auditAction: 'Action',
    auditActor: 'Actor',
    auditTarget: 'Target',
    auditResult: 'Result',
    auditAt: 'When',
    auditOk: 'Succeeded',
    auditFailed: 'Failed',
    jobs: 'Jobs',
    jobKind: 'Kind',
    jobState: 'State',
    jobStep: 'Step',
    jobDetail: 'Detail',
    jobDegradation: 'Degradation',
  },

  doctor: {
    title: 'Doctor',
    verdict: 'Verdict',
    findings: 'Findings',
    code: 'Code',
    message: 'Message',
    severity: 'Severity',
    run: 'Run check',
    running: 'Checking…',
    clean: 'No findings. The environment looks healthy.',
  },

  events: {
    connected: 'Live',
    reconnecting: 'Reconnecting',
    disconnected: 'Disconnected',
    lagged: 'Events were dropped; re-reading state',
    heartbeat: 'Heartbeat',
    kind: {
      'mihomo.log': 'Kernel log',
      'config.activated': 'Configuration activated',
      'config.rolled_back': 'Configuration rolled back',
      'subscription.updated': 'Subscription updated',
      'job.progress': 'Job progress',
      'job.finished': 'Job finished',
      heartbeat: 'Heartbeat',
      lagged: 'Events dropped',
    },
  },

  time: {
    justNow: 'just now',
    secondsAgo: '{{count}}s ago',
    minutesAgo: '{{count}}m ago',
    hoursAgo: '{{count}}h ago',
    daysAgo: '{{count}}d ago',
    inSeconds: 'in {{count}}s',
    inMinutes: 'in {{count}}m',
  },

  units: {
    bytes: '{{value}} B',
    kilobytes: '{{value}} KB',
    megabytes: '{{value}} MB',
    gigabytes: '{{value}} GB',
  },
} as const

/**
 * Widens every leaf to `string`, so the type describes the *shape* of the
 * translation tree and not its English text.
 *
 * Without this, `as const` would make each value its own literal type and
 * `zh.ts` would be required to contain the English strings verbatim. The point of
 * typing `zh` against `en` is to enforce the key set, not the words.
 */
type Widen<T> = T extends string ? string : { [K in keyof T]: Widen<T[K]> }

/** The canonical shape every locale must satisfy. */
export type Translation = Widen<typeof en>

/** A key path into the translation tree. */
export type TranslationKey = string
