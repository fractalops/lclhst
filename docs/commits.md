# Commit Message Guide

This guide describes how we write commit messages in this repo, and why. It is
adapted from Sumner Evans' *"Stop Using Conventional Commits"*
([sumnerevans.com](https://sumnerevans.com/posts/software-engineering/stop-using-conventional-commits/),
2 June 2026) and the discussion in its comment section.

**Short version:** use scope-prefixed commits, not Conventional Commits. Lead
with *what* changed, not *how*. Write a real body.

```text
<scope>: <description>

<body: what motivated the change, and why this solution>

<optional footer(s)>
```

## Why not Conventional Commits

Conventional Commits formats the subject as `<type>[optional scope]: <description>`,
where `type` is `fix`, `feat`, `chore`, `docs`, `refactor`, and so on. That gets
two things wrong.

### 1. It prioritises the wrong thing

`type` is elevated to the front; `scope` is made optional. That is backwards. The
people who actually read the log care about scope, not type:

- **Contributors** scan the log to see *what areas* moved since they last looked,
  or what might conflict with in-progress work.
- **Debuggers** look for commits that touched the component where a bug surfaced.
- **Incident responders** scan for changes near an outage.

In every case the *type* is irrelevant: a bug can be introduced by any change,
regardless of how it was labelled. "Having a commit without a scope is like having
a sentence without a subject."

### 2. Type is redundant and often restrictive

A good description already tells you the type. Worse, many changes resist a
single type — a change can be a refactor, a bugfix, *and* a new feature at once.
Forcing one label wastes scarce subject-line space and sometimes misleads.

## What we do instead: scoped commits

Follow the lead of Linux, FreeBSD, Git, Go, Node.js, and nixpkgs — all of which
prefix the subject with the scope of the change. See
[scopedcommits.com](https://scopedcommits.com) for the broader pitch.

### Scopes in this repo

The natural scope is the module the change lives in. Use the existing names so
the log stays greppable:

- `protocol:` — the wire protocol (hello frame, status bytes, name rules)
- `splice:` — bidirectional byte copying
- `ticket:` — ticket format and parsing
- `tunnel:` — iroh endpoint setup, ALPN, connection handling
- `target:` — the serving side (stream → local TCP)
- `edge:` — the receiving side (TLS, HTTP proxy, error pages)
- `tls:` — certificate generation and rustls config
- `cli:` — `src/main.rs`, argument parsing, output formatting
- `repo:` — Cargo.toml, licensing, project metadata
- `docs:`, `tests:`, `ci:` — non-library areas

When a change spans modules, prefix with the most specific shared scope, or
chain them as Linux does (`edge: tls: …`). If a commit genuinely touches
unrelated scopes, that is usually a sign it should be split.

Examples in our shape:

```text
protocol: hello frame, status bytes, name validation
edge: translate connect-failed status into a 502 page
tunnel: wait for endpoint.online() before minting tickets
```

## Write a real body

Every non-trivial body should answer two questions:

1. **What motivated this change?**
2. **Why is this a good solution?**

A one-line `fix: don't error on saving the form` strands future readers: was the
change a deliberate design call or an expedient hack? Even a sentence of context
prevents a Chesterton's-fence investigation later. Writing the body is also a
thinking aid — you often catch the problem while explaining the fix. Skip the
body only for genuinely trivial changes.

## Anti-patterns to avoid

- **Type prefixes** (`feat:`, `fix:`, `chore:`, `refactor:`). Drop the type; lead
  with scope.
- **Omitting the scope.** It's the subject of the sentence; don't make it optional.
- **Ticket number as the scope.** Put `Refs: #123` in a footer instead.
- **Checklist-compliance mindset.** A correctly-formatted but contentless subject
  is still useless.
- **Bundling unrelated changes**, then struggling to label them — split it.
- **Attribution trailers.** No `Co-Authored-By` noise; the history is the
  project's, not the tooling's.
