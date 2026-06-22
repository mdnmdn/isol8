# Home Config Wizard — design

A spec for an interactive generator that produces a `dev-home.toml` + `dev-agent`
launcher for running coding agents confined by **isol8** with a fake `$HOME`.

It exists because the current `dev-home.toml` was hand-tuned through a dozen
one-off fixes (node→mise, cargo/go caches, git/xcode-select, TLS trust, terminal
colors…). Every one of those fixes is an instance of the *same* small decision,
repeated per tool. The wizard makes that decision explicit, defaulted, and
repeatable.

---

## 1. Mental model

The sandbox is **deny-by-default**. `--home <dir>` swaps `$HOME` to a scratch dir
so the real home is invisible. Everything the agent is allowed to touch is then
granted back, one path/env/service at a time.

Two host-path tokens matter:

- `#HOME` → the **real** host home (`/Users/mdn`). Use to expose host installs.
- `~` → the **scratch** home (the `--home` dir). Use for disposable, isolated state.

> Gotcha baked into the file: profile **`env` values are not expanded** — `~` and
> `#HOME` are literal there, so env must be absolute paths. Only `[[paths]]`
> expand the tokens.

---

## 2. The four exposure modes (core abstraction)

Every host resource lands in exactly one mode:

| Mode | Meaning | isol8 mechanism | When |
|------|---------|-----------------|------|
| **MASK** | hidden / denied | *(no grant — default)* | real home, secrets, anything irrelevant |
| **RO** | read + execute, no writes | `access = "ro"` on `#HOME/...` | SDK/toolchain installs, system trust roots |
| **ISOLATE** (RW-scratch) | writable but disposable, **not** shared with host | `access = "rw"` on `~/...` + scratch env | global installs, agent state, throwaway caches |
| **SHARE** (RW-host) | writable **and** mapped to the real host dir | `access = "rw"` on `#HOME/...` + host env | dependency caches you want reused by the host (no re-download) |

`ISOLATE` vs `SHARE` is the central trade-off:

- **ISOLATE** = safe, reproducible, host can't be poisoned, but cold caches.
- **SHARE** = fast, warm caches, but the agent can write the host's cache (and,
  if the dir is the toolchain root, its binaries).

A specific `rw` grant **adds** write under a broader `ro` grant (isol8 grants are
additive) — that's how `SHARE` of `…/cargo` works while the rest of `works/rust`
stays RO.

---

## 3. A "tool" decomposes into roles

No tool is a single mode. Each one is a bundle of roles, and the wizard asks per
role (with smart defaults so most are silent):

| Role | Typical mode | Example |
|------|--------------|---------|
| **binary / SDK root** | RO | `#HOME/works/rust`, `#HOME/.sdkman` |
| **toolchain env** | (points at the RO root) | `RUSTUP_HOME`, `JAVA_HOME`, `GOROOT` |
| **dependency cache** | ISOLATE *or* SHARE | `~/.cargo` vs `#HOME/.../cargo`, `.m2`, `go/pkg/mod` |
| **global install target** | ISOLATE | `~/.npm-global`, `~/.local/bin` |
| **user config / creds** | RO seed or MASK | `.npmrc`, `settings.xml`, `.gitconfig` |
| **trust / system service** | RO + mach lookup | keychain/trustd, xcode-select `DEVELOPER_DIR` |

The wizard's real job: for each detected tool, pick the cache role mode (the only
genuinely per-user choice) and emit the rest from a template.

---

## 4. Provisioning strategy: host-provided vs sandbox-managed

Orthogonal to exposure mode — *where does the toolchain come from?*

- **HOST-PROVIDED** — mount the host install RO, point env at it. Uses the exact
  versions already on the machine. (current: rust/rustup, go, java/sdkman,
  python/pyenv.)
- **SANDBOX-MANAGED** — install fresh into the scratch home via a meta-tool
  (mise), fully host-independent. (current: node, after dropping nvm.)

Meta-tools split the same way:

| Meta-tool | Role here | Strategy |
|-----------|-----------|----------|
| **mise** | install new toolchains into scratch | sandbox-managed (the "extra deps" path) |
| **nvm** | host node manager | *dropped* — replaced by mise to avoid RO-home coupling |
| **sdkman** | host java/maven/gradle | host-provided, RO mount + env |
| **pyenv** | host python | host-provided, RO mount + env |
| **rustup** | host rust toolchains | host-provided, RO (`RUSTUP_HOME`) |

Rule of thumb the wizard encodes: **host-provided when the host manager is
already the source of truth; sandbox-managed (mise) when you want isolation or
the host manager fights the RO home** (nvm's global-install model did).

---

## 5. Tool-kind catalog with default modes

What the wizard ships as defaults (derived from the working config):

| Tool-kind | Detect | Binary/root | Cache default | Env emitted |
|-----------|--------|-------------|---------------|-------------|
| **git** | always (macOS shim) | RO CommandLineTools | — | `DEVELOPER_DIR`, `+apple-toolchain-core`, `+integrations/git` |
| **rust** | `~/works/rust` / `~/.cargo` | RO `works/rust` | **SHARE** `…/cargo` | `RUSTUP_HOME`, `CARGO_HOME` |
| **go** | `~/works/go` / `go` bin | RO `works/go` | **SHARE** `pkg/mod` + `go-build` | `GOROOT`, `GOMODCACHE`, `GOCACHE` |
| **java** | `~/.sdkman` | RO `.sdkman` | **SHARE** `.m2`, `.gradle` | `JAVA_HOME`, `MAVEN_HOME`, `GRADLE_HOME`, `SDKMAN_DIR` |
| **node** | `~/.nvm` / mise | **sandbox-managed** (mise) | ISOLATE `~/.npm-global` | `NPM_CONFIG_PREFIX` |
| **python** | `~/works/python/pyenv` | RO pyenv | ISOLATE (scratch wheels) | `PYENV_ROOT` |
| **bun/wasmer/foundry** | `~/.bun` etc. | RO | ISOLATE | `BUN_INSTALL`, `WASMER_DIR` |

Default *cache* policy is deliberately **per-kind, not global**: Java/Rust/Go
default to SHARE (big, expensive, trusted caches you already reuse); Node defaults
to ISOLATE (global installs are cheap and you don't want the agent writing host
node_modules). The wizard surfaces this as one toggle per kind.

---

## 6. High-level / service tools (different axis)

Tools like **docker** aren't a filesystem toolchain — they're access to a daemon
socket + network + maybe a VM. Model them as a separate "service access" choice:

| Tool | OFF (default) | HOST | ISOLATED |
|------|---------------|------|----------|
| **docker / colima** | no socket | mount host socket RW (`+integrations/docker`) — shares host containers | rootless/in-VM (heavy) |
| **kubectl** | no kubeconfig | RO `~/.kube` + network | per-context scratch config |
| **gcloud/aws** | MASK creds | RO creds + token cache RW | scoped service-account file |
| **ssh** | MASK keys | RO `~/.ssh` + agent socket | scratch key |

These default **OFF** — they're the highest-blast-radius grants, so opt-in only.

---

## 7. Wizard flow

```
create  → new scratch home from scratch
update  → edit an existing dev-home.toml (re-run, diff, apply)
```

### Step 0 — target
- Home dir? (default `~/works/_dev-home`)
- Which agents? (claude, codex, gemini, … → generates `profiles/<agent>-skip.toml`)

### Step 1 — detect
Scan the host: `~/.sdkman`, `~/.nvm`, `~/works/{rust,go,python}`, `mise`, `pyenv`,
`~/.cargo`, `docker`/`colima` socket, `~/.kube`, cloud SDKs. Present a checklist
of what was found.

### Step 2 — per tool-kind (the core loop)
For each detected kind, one question, defaulted:

> **Rust toolchain — how should the agent use it?**
> 1. **Share host cache (recommended)** — RO install, RW shared `~/.cargo`. Warm cache, fast.
> 2. **Isolated cache** — RO install, RW scratch cache. Safe, re-downloads.
> 3. **Read-only** — can build against it but no cache writes (needs vendored deps).
> 4. **Sandbox-managed** — ignore host, install via mise.
> 5. **Disable / mask** — hide it entirely.

(Same five-way question for each kind; default differs per kind per §5.)

### Step 3 — meta-tools
- New deps via **mise** into scratch? (default yes — needs scratch `.local/state`
  pre-created + trust grant; see §8.)
- Keep host nvm/sdkman/pyenv as host-provided? (default: yes for sdkman/pyenv,
  **no** for nvm.)

### Step 4 — services (opt-in)
docker / kubectl / cloud / ssh → OFF | HOST | ISOLATED. Default OFF, with an
explicit "this grants the agent your host daemon/creds" warning.

### Step 5 — terminal & trust (defaulted, rarely asked)
- Forward terminal env (TERM/COLORTERM/LANG…)? default **yes**.
- TLS trust for rustls tools (keychain/trustd)? default **yes**.
- macOS git shim (`DEVELOPER_DIR`)? default **yes**.

### Step 6 — emit
Generate `dev-home.toml` (paths + env + requires) and confirm/patch `dev-agent`
(PATH prepend, `--env-pass`, skip layers). On `update`, show a diff first.

---

## 8. Mode → output mapping (what the generator writes)

| Decision | Emits |
|----------|-------|
| RO install | `[[paths]] path="#HOME/..." access="ro"` |
| ISOLATE cache | `[[paths]] path="~/..." access="rw"` + scratch-absolute env + **pre-create dir** |
| SHARE cache | `[[paths]] path="#HOME/..." access="rw"` + host-absolute env |
| MASK | *(nothing)* |
| sandbox-managed node | `+toolchains/runtime-managers`, mise dirs pre-created, shims on PATH |
| service: docker HOST | `+integrations/docker`, socket grant |
| terminal | `dev-agent`: `--env-pass TERM COLORTERM TERM_PROGRAM … LANG LC_*` |
| TLS trust | `+integrations/keychain` (under `--home` only system roots are read) |
| git shim | `+toolchains/apple-toolchain-core` + `DEVELOPER_DIR` env |

**Pre-creation matters:** isol8 `rw` on `~/.foo` lets you write *into* `.foo`, but
creating `.foo` needs its parent writable. The generator must `mkdir -p` every
ISOLATE dir in the scratch home (we hit this with mise's `.local/state` and
`.npm-global`).

---

## 9. Candidates to upstream into isol8 builtins

Several of this session's fixes are **generic, not user-specific** — they belong
in isol8's builtin profiles so every user gets them free, shrinking `dev-home.toml`
to just the machine-specific install paths + cache mode choices.

| Fix (currently in our profile/launcher) | Should live in | Why it's generic |
|------------------------------------------|----------------|------------------|
| `--env-pass TERM/COLORTERM/TERM_PROGRAM/LANG/LC_*` | `base` or `macos/system-runtime` | every interactive CLI needs terminal env; `TERM=dumb` default breaks all color |
| keychain/trustd grant for TLS | a `tls-trust` layer required by `toolchains/rust` + `runtime-managers` | any rustls tool (cargo, mise) can't fetch without it; surprising to debug |
| `SSL_CERT_FILE=/etc/ssl/cert.pem` | `macos/system-runtime` | openssl-based tools need a CA path in a stripped env |
| `DEVELOPER_DIR` + apple-toolchain for `/usr/bin/git` | `integrations/git` (macOS) | `git` is *always* the xcode-select shim on macOS; git profile that doesn't run git is a footgun |
| mise scratch dirs pre-created + `.local/state` writable | `toolchains/runtime-managers` | mise is unusable out-of-box otherwise (`Operation not permitted`) |
| `NPM_CONFIG_PREFIX` → scratch + grant + `.local/bin` RW | `toolchains/node` | `npm i -g` fails by default against an RO host node |
| per-agent skip-flag rewrites (`*-skip.toml`) | builtin agent profiles, opt-in rewrite | the flags (`--dangerously-skip-permissions`, `--yolo`, …) are stable per agent |

**Net effect if upstreamed:** `dev-home.toml` collapses to (a) the host install
roots to mount RO, (b) per-kind cache mode (ISOLATE/SHARE), (c) the absolute env
pointers — which is exactly what the wizard would ask. Everything in §5–§8's
"trust/terminal/git/mise" rows becomes a default the user never sees.

---

## 10. Open questions / decisions to pin down

1. **SHARE granularity for cargo** — current SHARE is the whole `CARGO_HOME`
   (includes `bin/`). Narrow to `registry` + `git` + root lock files to keep
   toolchain binaries RO? (Wizard option: "share cache only" vs "share home".)
2. **Format** — wizard output is a generated `dev-home.toml`. Keep it hand-editable
   (comments preserved) or treat it as generated-only with a separate
   `home-config.yaml` source of truth?
3. **Detection depth** — scan `$PATH` + known dirs, or parse the host shell rc for
   `*_HOME`/`*_DIR` exports (where these env values actually come from)?
4. **Per-project overrides** — `dev-agent` already takes `DEV_AGENT_PROJECT`; should
   a project carry its own cache-mode overrides (e.g. force ISOLATE for an
   untrusted repo)?
5. **Multi-OS** — `#HOME` tokens are portable but env paths are absolute/macOS.
   Wizard re-derives env per machine; the install-root list is the portable part.

---

## 11. Declarative registry — `home-config-tools.yaml`

The wizard is data-driven: the tool catalog (§5) lives in **`home-config-tools.yaml`**,
not in wizard code. Adding/altering a tool is a data edit. Key schema decisions
that emerged while modeling git/java/node/rust/go/mise/docker:

- **One mode vocabulary, per *resource*** — `mask | ro | isolate | share`. A tool
  is a list of `resources` (roles: binary/cache/global/config/state/…), each with
  its own `default_mode` + `allowed_modes`. "Partial RW (cache only)" is not a
  special case — it's `install: ro` + `cache: share`, bundled as the `cache-rw`
  **preset**. Presets (`hidden`/`read-only`/`cache-rw`/`full-rw`/`managed`) are the
  one-click UI; per-resource overrides are the fine-tune.

- **Home-relative paths + mode decides the base.** A resource path like `.m2` or
  `.cargo/registry` resolves under `#HOME` (real) for `ro`/`share` and under `~`
  (scratch) for `isolate`. `base: system` opts out to an absolute path. So a
  resource is just *path + mode* — no per-mode path duplication.

- **System defaults, detection overrides** (this is what keeps the registry
  portable). Paths are the upstream defaults — `~/.cargo`, `~/.rustup`, `~/go`,
  `go env GOROOT`, `~/.sdkman` — never one machine's layout. A non-standard install
  is discovered at generation time via:
  - `path_from_env: CARGO_HOME` on a resource (host env value replaces the default path), and
  - `{{detected:cargo_home}}` env placeholders (`$CARGO_HOME || ~/.cargo`, `go env GOROOT`, …).
  The wizard substitutes `{{host_home}}` / `{{scratch_home}}` / `{{detected:*}}` at
  generation time — isol8 env values are literal, so this *must* happen before emit.

- **Mode-dependent env via `select`/`by`.** `CARGO_HOME`/`GOMODCACHE` flip between
  the host dir (share) and a scratch dir (isolate) keyed by the cache resource's
  chosen mode — so the env follows the path automatically.

- **Globals** (`terminal`, `tls-trust`, `macos-git-shim`) model §9's cross-cutting
  fixes as default-on toggles → exactly the upstreaming candidates.

- **Services** (docker) reuse the mode enum (`mask`==off, `share`==mount host
  socket RW) under a `services:` block with `socket` candidates + conditional
  `requires`. Default `hidden` — highest blast radius, opt-in only.

This resolves open question §10.2: the source of truth is the YAML registry +
per-home answers; `dev-home.toml` is a generated artifact.
