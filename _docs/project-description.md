# isol8 — Cross-Platform Agent Sandbox — Rust Implementation Specification

> A lightweight, cross-platform isolation sandbox toolkit for AI coding agents and CLI tools — deny-by-default — generalizing the macOS `sandbox-exec` model (cf. *Agent Safehouse*) to Linux, WSL2, and Windows.
>
> This document specifies functional requirements, per-OS feasibility, **concrete Rust implementation references and patterns (as of June 2026)**, and an expanded roadmap including networking details and further refinements.

**Status:** Draft requirements + Implementation guide  
**Scope:** Userspace process isolation, filesystem access control (primary focus), environment isolation, tiered network isolation (secondary), HOME-path replacement (first-class).  
**Primary Targets:** Linux (Landlock + namespaces) and macOS (Seatbelt). Windows deferred but planned.  
**Language:** Rust (single-binary CLI tool with platform backends).

---

## 1. Target Platforms

| ID | Platform | Notes | Rust Backend Approach |
|---|---|---|---|
| **macOS** | macOS 12+ | Baseline reference: Seatbelt / `sandbox-exec`. | Policy text generation + `sandbox-exec` invocation (or `sandbox_init` FFI). |
| **Linux** | Kernel ≥ 5.13 | Namespaces, Landlock, seccomp, cgroups v2. | `landlock` crate + optional `nix` user/mount namespaces; `pasta` for networking. |
| **WSL2** | WSL2 on Win 10/11 | Real Linux kernel; inherits most Linux capability with caveats. | Same as Linux (with interop/DNS caveats). |
| **Windows** | Windows 10/11 | Native (no WSL); AppContainer / Job Objects / WFP. | `windows` crate + `rappct` / `win32job-rs` (later phase). |

**Legend:** ✅ fully feasible · 🟡 feasible with limitations · 🔴 not feasible / requires heavy workaround.

---

## 2. Functional Requirements

### R1 — Userspace Process Isolation

**Requirement.** Wrap an arbitrary command so it runs as an isolated, unprivileged process with a restricted view of the system. No kernel modules, no VM, no persistent daemon required for the baseline. The wrapper itself should run unprivileged where possible, escalating only for tiers that demand it (see R5).

**Sub-requirements.**
- R1.1 — Launch a target command with its children confined to the same policy.
- R1.2 — Prevent the confined process from escalating privilege (no new privs).
- R1.3 — Optional resource limits (CPU, memory, PID count).
- R1.4 — Clean teardown: confinement ends when the process tree exits.

| OS | Feasibility | Mechanism | Limitations |
|---|---|---|---|
| macOS | ✅ | `sandbox-exec` (Seatbelt) + `posix_spawn`; `env -i` | Seatbelt is officially deprecated though still functional; policy language undocumented. No per-process resource caps via Seatbelt (use `ulimit`/`launchd`). |
| Linux | ✅ | User namespaces + `bwrap`/`unshare`; `PR_SET_NO_NEW_PRIVS`; seccomp; cgroups v2 for R1.3 | Requires `kernel.unprivileged_userns_clone=1` on some distros (Debian/Arch hardening may disable it). |
| WSL2 | 🟡 | Same as Linux | Unprivileged userns sometimes disabled by default; cgroup v2 delegation may be partial depending on distro init. Generally works once userns is enabled. |
| Windows | 🟡 | AppContainer (per-process SID + capability set) + Job Objects (R1.3) + `CREATE_BREAKAWAY_FROM_JOB` discipline | Very different model; no `env -i` analog (clear env at `CreateProcess`). No single CLI primitive — requires Win32 API orchestration. Some console/CLI tools misbehave inside AppContainer. |

#### Rust Implementation Notes & References (R1)
- **Linux (strongest)**: Use `nix` crate for `unshare(CLONE_NEWUSER | CLONE_NEWNS | ...)` + `prctl(PR_SET_NO_NEW_PRIVS)`. Combine with Landlock for filesystem. Resource limits via `setrlimit` or `cgroups` crate. Clean teardown automatic on `exec` + `wait`.
- **Key crates**: `landlock` (for fs rules), `nix` (syscalls), `seccompiler` or raw BPF for seccomp profiles.
- **Reference projects**:
  - [sandbox-rs](https://github.com/ErickJ3/sandbox-rs) — unprivileged mode with user namespaces + Landlock + seccomp + setrlimit. Excellent starting point or reference.
  - Production patterns in AI coding agents (e.g., similar to `codex-rs` Linux sandbox helper binary that applies restrictions before `execvp`).
- **macOS**: `std::process::Command` spawning `sandbox-exec`. Children inherit via Seatbelt. Use `PR_SET_PDEATHSIG` equivalent via `posix_spawn` attributes or `libproc`.
- **Windows (later)**: `CreateJobObject` + `AssignProcessToJobObject` via `win32job-rs` or `windows` crate. AppContainer via `rappct` or manual token creation.
- **General**: The tool binary itself stays unprivileged; only N3 networking helper escalates temporarily.

### R2 — Selective Path Access (no / ro / rw)

**Requirement.** Per-path access control with three levels: **no access**, **read-only (ro)**, **read-write (rw)**. Default is deny. The working directory is granted rw; its ancestors get metadata-only (stat) access for path resolution; system runtime paths get ro; everything else is denied unless explicitly granted.

**Sub-requirements.**
- R2.1 — Deny-by-default filesystem; explicit grants only.
- R2.2 — Three tiers per path: none / ro / rw.
- R2.3 — Ancestor metadata-only (stat) access for path resolution without content read.
- R2.4 — Grants composable as layered "profiles" (system, per-toolchain, per-feature).
- R2.5 — Custom grants at invocation (`--add-dirs-ro`, `--add-dirs`).

| OS | Feasibility | Mechanism | Limitations |
|---|---|---|---|
| macOS | ✅ | Seatbelt `(allow file-read*)` / `(allow file-write*)` with path/subpath/regex literals; `(deny default)` | Metadata-only (R2.3) approximated with `file-read-metadata`. Regex rules easy to over-grant. |
| Linux | ✅ | **Two interchangeable models:** (a) **bubblewrap** constructs a new mount view — bind nothing, then `--ro-bind`/`--bind` exactly the grants; (b) **Landlock** LSM restricts the *real* filesystem with `LANDLOCK_ACCESS_FS_*` rights. | Landlock ≥ 5.13 has no "metadata-only" right (R2.3) until later ABI versions refine granularity — ancestor stat is coarse; bubblewrap handles R2.3 naturally by simply not binding the content. Landlock can't restrict already-open fds. |
| WSL2 | 🟡 | Same as Linux | Paths under `/mnt/c` (Windows drives via 9P/drvfs) get weaker Landlock guarantees; keep confined work on the Linux filesystem. bubblewrap binds of `/mnt/c/...` work but performance and semantics degrade. |
| Windows | 🟡 | AppContainer filesystem ACLs (capability SID must be granted on each object) + per-object `SetNamedSecurityInfo`; deny via absence of grant | No clean "construct a fresh view" like bind mounts (short of containers). rw/ro enforced via ACE on each path; ancestor metadata (R2.3) is implicit (traverse permission). Granting is per-ACL, more verbose and stateful than bind mounts. WSL paths and network shares complicate ACL reasoning. |

#### Rust Implementation Notes & References (R2 — Primary Focus)
- **Linux (recommended primary path)**: 
  - Pure Landlock (lightweight, no ns required for basic fs isolation): `landlock` crate v0.4+ (`Ruleset::new()`, `add_rule(PathBeneath::new(fd, AccessFs::from_bits(...)))`, `restrict_self()`).
  - Hybrid (for better R2.3 + HOME robustness): `nix` user + mount namespaces + bind mounts (or fall back to invoking `bwrap` for MVP).
  - Ancestor metadata: Landlock provides coarse support; use bind-mount approach or accept and document (most CLI tools work well).
  - Already-open fds: Mitigate with seccomp + `no_new_privs` or pre-policy open handling.
- **Key crate**: [landlock](https://github.com/landlock-lsm/rust-landlock) — safe abstraction over Landlock syscalls. Also [sandbox-landlock](https://docs.rs/sandbox-landlock).
- **macOS**: Generate Seatbelt policy text with `(allow file-read* (subpath "/allowed"))` etc. Many production examples exist (e.g., policy generation in Rust agent runtimes using `sandbox-exec -f` or `-p`).
- **Profile composability (R2.4/R2.5)**: Define `Profile` struct with `Vec<PathGrant>` (path, access: No/Ro/Rw). Merge layers deny-first. Render per-backend.
- **Custom grants**: CLI flags parsed into the same `Profile` struct before rendering.

### R3 — Environment Isolation

**Requirement.** The confined process starts from a sanitized environment to prevent host secrets (API keys, tokens) leaking in. Default env is a minimal allowlist; opt-in passthrough is explicit.

**Sub-requirements.**
- R3.1 — Start from minimal env: `HOME`, `PATH`, `SHELL`, `TMPDIR`, `USER`, `LOGNAME`, `PWD`.
- R3.2 — `--env-pass NAMES` to pass named host vars through.
- R3.3 — `--env=FILE` to source overrides from a file.
- R3.4 — `--env` to inherit the full host env (escape hatch).
- R3.5 — Profile-supplied env defaults (no-override merge).

| OS | Feasibility | Mechanism | Limitations |
|---|---|---|---|
| macOS | ✅ | `/usr/bin/env -i KEY=VAL ... cmd` | None of significance. |
| Linux | ✅ | `env -i` or `bwrap --clearenv --setenv ...` | None of significance. |
| WSL2 | ✅ | Same as Linux | **WSL interop injects `WSLENV`/`PATH` Windows entries**; sanitize aggressively or Windows `PATH` segments leak in. Disable interop in `wsl.conf` if undesired. |
| Windows | 🟡 | Pass an explicitly built environment block to `CreateProcess` (lpEnvironment); omit inherited vars | No `env -i` CLI; must construct the block programmatically. Some Windows APIs read config from registry/`%APPDATA%` rather than env, so env sanitization alone leaks less but also controls less. |

#### Rust Implementation Notes & References (R3)
- Build `HashMap<String, String>` or `std::env::Vars` filtered allowlist.
- Always set `HOME` to replacement value **before** other processing.
- On Linux: `Command::new(...).env_clear().envs(minimal_map)`.
- On macOS: Same, passed through to `sandbox-exec`.
- WSL2: Explicitly filter `WSLENV` and Windows `PATH` segments.
- Profile can supply default env vars (merged without override unless `--env` escape).

### R4 — HOME-Path Replacement (First-Class)

**Requirement.** Provide the confined process with a **replaced `$HOME`** as a first-order operation, applied *before* any other path resolution or profile rendering. Rather than only restricting the real home directory, the sandbox can substitute an alternate home (e.g., a scratch or per-session directory), so that tools writing to `~/.config`, `~/.cache`, `~/.ssh`, credential stores, shell init files, etc. are transparently redirected to a controlled location.

**Rationale.** Many toolchains derive dozens of paths from `$HOME`. Replacing `$HOME` at the outset collapses a large class of grants into one decision and prevents accidental reads/writes to the user's real dotfiles. It also enables disposable, reproducible, per-session home directories.

**Sub-requirements.**
- R4.0 — Replacement is **opt-in**: with no `--home` and no profile enabling `home_replace`, the effective home is the user's real home (so a command's own binary/config under `~` stay reachable). R4.2–R4.6 govern behaviour *when a replacement is active*.
- R4.1 — Resolve an alternate home directory (provided, or auto-created per session/scratch).
- R4.2 — Apply the replacement **first**, before profile rendering / path-grant computation, so all downstream `$HOME`-derived grants target the replacement.
- R4.3 — Set `HOME` (and OS equivalents) in the sanitized env to the replacement.
- R4.4 — Optionally seed the replacement home with selected entries from the real home (allowlisted ro copies or binds: e.g., `~/.gitconfig`, a scoped `~/.ssh` subset).
- R4.5 — Ensure the real home is *not* granted by default once a replacement is active (deny real `$HOME` unless explicitly re-added).
- R4.6 — Keep `$HOME`-derived ancestor metadata access consistent with the replacement, not the real home.

| OS | Feasibility | Mechanism | Limitations |
|---|---|---|---|
| macOS | 🟡 | Set `HOME=<alt>` in `env -i`; render Seatbelt policy against the alternate. **Caveat:** macOS also resolves the home via the user record (`getpwuid`), not only `$HOME`. APIs like `NSHomeDirectory()` / `confstr(_CS_DARWIN_USER_DIR)` and `~`-expansion in some daemons ignore `$HOME`. | `$HOME` covers most CLI tools and shells, but Cocoa/`NSHomeDirectory` and sandbox container redirection (`~/Library/Containers`) do not honor `$HOME`. Full redirection of GUI/Cocoa paths isn't achievable via `$HOME` alone. |
| Linux | ✅ | Set `HOME=<alt>` **and** bind-mount the alternate over the real home path for belt-and-suspenders: `bwrap --bind <alt> /home/<user>` so even `getpwnam`-derived `~` resolves into the controlled dir. | `getpwnam`/NSS still reports the *original* home string; tools that read `/etc/passwd` directly (rare) see the real path — mitigated by the bind-over-real-home approach. Bind-over-home requires the mount-namespace path (bubblewrap), not pure Landlock. |
| WSL2 | ✅ | Same as Linux (set `HOME`, optionally bind-over-home) | Windows-interop tools invoked from WSL may resolve `%USERPROFILE%` on the Windows side, bypassing the Linux `$HOME`. Keep confined work Linux-side. |
| Windows | 🟡 | Set `USERPROFILE`, `HOMEDRIVE`, `HOMEPATH`, `APPDATA`, `LOCALAPPDATA` in the process env block to the alternate location. | Windows home is governed by **multiple** vars plus registry (`ProfileImagePath`) and the token's profile; many apps read `SHGetKnownFolderPath` which consults the token/registry, **not** env. So env-based redirection is partial; full redirection needs a distinct user profile or container. Higher effort than POSIX. |

**Design note (all OSes).** Because `$HOME` is the highest-leverage path, R4 is specified as the *first* resolution step: the policy engine computes the effective home, then renders every `$HOME`-relative grant against it. On Linux/WSL the bind-over-real-home technique makes the replacement robust against non-`$HOME` resolution; on macOS and Windows the replacement is best-effort for CLI tooling and incomplete for native GUI frameworks.

#### Rust Implementation Notes & References (R4 — Primary Focus)
- **Core logic (all platforms)**: Resolve `effective_home` first (CLI flag, auto `/tmp/isol8-$$-home`, or profile). Then build all path grants relative to it. Set in minimal env map.
- **Linux robust implementation**:
  - Landlock-only: Rules allowing full access under `effective_home`, deny (or explicit) on real `$HOME`.
  - Recommended hybrid: Use `nix` to enter user + mount namespace, then `mount::bind` the alt home over real home path (or use overlayfs for seeding). This satisfies R4.6 perfectly.
  - Seeding (R4.4): Copy allowlisted files/dirs (e.g., `~/.gitconfig`, limited `~/.ssh`) read-only into the scratch home before exec.
- **macOS**: `HOME` env + policy rendered against alt paths. Document Cocoa limitations.
- **Auto-scratch**: Use `tempfile::tempdir` or controlled path under `/tmp` or `XDG_RUNTIME_DIR`.
- **Profile integration**: `home_replace: { enabled: true, seed: ["~/.gitconfig"] }` in TOML layers.

### R5 — Tiered Network Isolation

**Requirement.** Multiple selectable tiers of network confinement, from none to kernel-enforced domain filtering, so the user can match the threat model (accidental vs. cooperative vs. adversarial) and the available privilege.

**Tiers.**

| Tier | Name | Mechanism (concept) | Root / CAP needed | Stops deliberate bypass? |
|---|---|---|---|---|
| **N0** | None | Share host network / plain NAT | no | n/a |
| **N1** | Cooperative proxy | Filtering proxy + `HTTP(S)_PROXY`/`NO_PROXY` env; allow/deny by domain | no | ❌ (process can ignore env) |
| **N2** | Rootless enforced | Userspace net stack (pasta/slirp4netns) where the proxy is the **only** reachable endpoint; no host route | no | 🟡 mostly (userspace boundary) |
| **N3** | Rooted enforced | Dedicated net namespace + veth + nftables `tproxy`/`redirect`; all egress forced through proxy, all else dropped | yes (`CAP_NET_ADMIN`) | ✅ kernel-enforced |

**Sub-requirements.**
- R5.1 — Domain **allowlist/blocklist** enforced at a filtering proxy.
- R5.2 — Two inspection depths: **hostname-only** (SNI / `CONNECT` host — no CA, no decryption) and **full MITM** (local trusted CA, path/method/header/body rules + logging).
- R5.3 — Network policy expressed as composable profile layers (a `github` layer adds `github.com`, `*.githubusercontent.com`; an `npm` layer adds `registry.npmjs.org`; etc.). Effective allowlist = union of enabled layers.
- R5.4 — DNS control: confined resolver answers only for allowlisted domains, or proxy-side name resolution, to close the DNS side channel.
- R5.5 — IPv6 must be denied or equally redirected (no v6 bypass).
- R5.6 — Privileged tier uses a **small privileged helper** that sets up plumbing then drops privilege before exec.
- R5.7 — Auto-select the strongest tier the environment supports; graceful fallback N3 → N2 → N1 → N0.

| OS | Feasibility per tier | Mechanism | Limitations |
|---|---|---|---|
| macOS | N0 ✅ · N1 ✅ · N2 🔴 · N3 🔴 | Seatbelt can allow/deny by host/port at the socket layer but **cannot filter by domain**; N1 via proxy env + a userspace filtering proxy. No netns/veth/nftables equivalent. | No transparent kernel-enforced redirect available to userspace. "Enforced" tiers (N2/N3) require either a network extension (NE provider, needs entitlement + signing) or a system-level packet filter (`pf`) with root — heavyweight, not equivalent to nftables tproxy. Domain filtering only at the proxy, and the proxy can be bypassed by direct sockets unless `pf` blocks all non-proxy egress (root). |
| Linux | N0 ✅ · N1 ✅ · N2 ✅ · N3 ✅ | N2: bubblewrap `--unshare-net` + pasta/slirp4netns pointed at the proxy. N3: gateway netns + veth + nftables `tproxy`. Proxy: mitmproxy/Squid (SNI peek or ssl_bump). | N2 boundary is userspace-strong, not kernel-hard. N3 needs `CAP_NET_ADMIN`. MITM (R5.2) requires per-toolchain CA trust (see R5 caveats below). |
| WSL2 | N0 ✅ · N1 ✅ · N2 ✅ · N3 🟡 | Same as Linux; netns/veth/nftables/userns all present in WSL2 kernel. | WSL NAT layer sits between VM and Windows host; `/etc/resolv.conf` is auto-generated (disable via `wsl.conf generateResolvConf=false` to control DNS, R5.4). N3 typically available since WSL often has passwordless root, but rules live inside the WSL VM only. |
| Windows | N0 ✅ · N1 ✅ · N2 🟡 · N3 🟡 | N1: proxy env (`HTTP(S)_PROXY`) or WinINET/WinHTTP proxy config. Enforced tiers via **Windows Filtering Platform (WFP)** callout filters keyed to the AppContainer SID, or per-app loopback/firewall rules. | No netns/nftables model; WFP is the closest enforcement layer but is a different programming surface (kernel/user callouts, requires admin). AppContainer can be denied general network capability and granted only loopback to a local proxy — a viable N2/N3-ish design, but more bespoke than Linux. MITM CA install into the system store affects the whole machine unless scoped. |

**R5 cross-cutting caveats (apply wherever MITM / enforced tiers are used).**
- **Per-toolchain trust stores (MITM only).** System CA store is not enough: Node (`NODE_EXTRA_CA_CERTS`), Python/requests (`REQUESTS_CA_BUNDLE`/`SSL_CERT_FILE`), Go, Java keystore, Git (`http.sslCAInfo`), curl each may need their own pointer. Hostname-only filtering (R5.2) avoids all of this.
- **Cert pinning** breaks under MITM; provide per-host decrypt-exempt tunneling.
- **DNS exfiltration** remains open unless R5.4 is enforced.
- **IPv6** is a trivial bypass if forgotten (R5.5).

#### Rust Implementation Notes & References (R5 — Secondary Focus)
- **N0/N1 (MVP)**: Simple env vars (`HTTP_PROXY`, `HTTPS_PROXY`, `NO_PROXY`) + optional lightweight Rust proxy (using `hyper` + `hyper-proxy` or custom `axum`/`tower` based filter). Domain lists from profile layers.
- **N2 (Linux recommended)**: Use `nix` to unshare network namespace, then spawn `pasta` (from `passt` project) as the userspace stack pointing at the filtering proxy. `pasta` is modern, high-performance, supports IPv6 natively, and is rootless. Reference: Used in Podman, and mentioned in Rust sandbox projects like Zerobox and redoubtful.
- **N3**: Small dedicated helper binary (compiled alongside main tool). Use `setcap cap_net_admin+ep` on it. It creates netns + veth pair, installs nftables `tproxy`/`redirect` rules, starts proxy, drops caps, then `exec`s the main sandboxed process into the prepared ns.
- **Proxy choices**:
  - Hostname/SNI only (recommended default): Easier, no CA issues.
  - Full MITM: More complex (need to generate CA, inject per-toolchain env vars like `NODE_EXTRA_CA_CERTS`, `REQUESTS_CA_BUNDLE`, `GIT_SSL_CAINFO`, etc.). Provide per-host exemptions for pinning.
- **DNS (R5.4)**: For enforced tiers, either proxy does resolution or use a restricted resolver (e.g., `unbound` config or simple Rust stub resolver that only answers allowlisted domains).
- **IPv6 (R5.5)**: `pasta` handles well; for pure Landlock or Seatbelt, explicitly deny or redirect.
- **Auto-select (R5.7)**: Probe for `pasta` binary, userns, `CAP_NET_ADMIN`, Landlock ABI version. Report effective tier on startup (`--verbose` or always in logs).
- **Future refinement**: Pure-Rust userspace network stack option (more complex) or integration with `slirp4netns` as fallback.

### R6 — Composable Profile Model (Cross-Cutting)

**Requirement.** All of R2–R5 grants are expressed as **layered, numbered profiles** resolved deny-first, mirroring the Safehouse model: base → system runtime → network → toolchains → shared → core integrations → opt-in integrations (`--enable`) → per-agent/app (auto-detected) → workdir grants → custom grants → appended profiles. A profile may contribute filesystem grants, env defaults, and network allowlist domains simultaneously.

| OS | Feasibility | Notes |
|---|---|---|
| macOS | ✅ | Native fit (`.sb` concatenation). |
| Linux | ✅ | Compose bwrap arg fragments / Landlock rulesets + nftables/proxy allowlist fragments per profile. |
| WSL2 | ✅ | Same as Linux. |
| Windows | 🟡 | Conceptually portable, but each layer maps to heterogeneous mechanisms (ACLs + AppContainer caps + WFP filters + env block), so a profile's "render" is more complex than concatenating text. |

#### Rust Implementation Notes & References (R6)
- Define `ProfileLayer` and `Profile` structs with `Vec<PathGrant>`, `EnvMap`, `NetworkAllowlist`.
- Merge function: iterate layers deny-first, union allows.
- Rendering: `fn render_linux(&self) -> LandlockRuleset` and `fn render_macos(&self) -> String` (Seatbelt policy text).
- CLI: `--enable rust,node,github` loads named layers from a profiles directory or embedded defaults.
- Auto-detection: Simple heuristics or explicit `--profile-for cargo` etc.
- Storage: TOML files for user profiles; built-in defaults compiled into binary (or separate data dir).

---

## 3. Privilege Model (Cross-Cutting)

- Default: **run unprivileged.** Tiers N0, N1, N2 and all of R1–R4 (Linux/WSL) need no root.
- **N3 enforced networking** needs `CAP_NET_ADMIN` (Linux/WSL) or admin + WFP (Windows). Prefer file-capability (`setcap cap_net_admin+ep`) on a **small helper** over full root.
- Helper pattern (R5.6): privileged helper creates the gateway namespace / installs nftables (or WFP filters), starts the proxy, **drops privilege**, then execs the agent unprivileged in the prepared namespace. The rooted tier reuses ~all rootless machinery, swapping pasta for veth+nftables.
- **Capability probing:** detect `CAP_NET_ADMIN`/admin, userns availability, Landlock ABI, pasta/slirp4netns presence; auto-select the strongest supported tier and report the effective tier to the user.

**Rust notes**: Use `caps` crate or `nix::sys::capability` for probing/dropping. The main binary never needs root except when invoking the N3 helper.

---

## 4. Per-OS Feasibility Summary (Original + Rust Status)

| Requirement | macOS | Linux | WSL2 | Windows |
|---|---|---|---|---|
| R1 Process isolation | ✅ (Seatbelt) | ✅ (Landlock + ns) | 🟡 | 🟡 (later) |
| R2 Path no/ro/rw | ✅ | ✅ (primary) | 🟡 | 🟡 (later) |
| R3 Env isolation | ✅ | ✅ | ✅ | 🟡 |
| R4 HOME replacement | 🟡 | ✅ (hybrid ns+Landlock) | ✅ | 🟡 |
| R5 Net N0/N1 | ✅ | ✅ | ✅ | ✅ |
| R5 Net N2 | 🔴 | ✅ (pasta) | ✅ | 🟡 |
| R5 Net N3 | 🔴 | ✅ (helper) | 🟡 | 🟡 |
| R6 Composable profiles | ✅ | ✅ | ✅ | 🟡 |

**Reading the summary.** Linux is the strongest and primary target for the initial Rust implementation (especially path + HOME features). macOS follows closely via proven Seatbelt patterns. Windows is deferred.

---

## 5. Open Questions / Decisions to Lock (Original + Rust-Specific)

1. **Default network tier** per platform (likely N1 cooperative as the safe, frictionless default; N2/N3 opt-in).
2. **MITM vs. hostname-only as default** (recommend hostname-only; MITM behind an explicit flag because of trust-store breakage).
3. **HOME replacement default** — on by default with a scratch home, or opt-in? (Affects how many real-home grants must be re-added.)
4. **Windows scope** — is native Windows a first-class target, or is WSL2 the supported Windows path with native Windows deferred?
5. **macOS enforced networking** — accept N0/N1 only, or invest in a Network Extension for parity?
6. **(New) Pure Landlock vs hybrid mount-ns for Linux** — pure for simplicity/low overhead; hybrid for perfect R4.6 and ancestor metadata.
7. **(New) Proxy implementation** — pure Rust (hyper-based) vs orchestrate external binary (mitmproxy/pasta) for MVP speed.
8. **(New) Profile storage & distribution** — embedded defaults + user TOML dir, or full config management crate.

---

## 6. Rust Ecosystem References & Building Blocks (June 2026)

### Core Crates for Linux Path Isolation (R2/R4 Primary)
- **[landlock](https://github.com/landlock-lsm/rust-landlock)** (v0.4.5+): Official-style safe Rust bindings for Landlock LSM. `Ruleset`, `PathBeneath`, `AccessFs`. Perfect for deny-by-default ro/rw rules. Low overhead, unprivileged.
- **[sandbox-landlock](https://docs.rs/sandbox-landlock)** and **[sandbox-rs](https://github.com/ErickJ3/sandbox-rs)**: Higher-level unprivileged sandbox using Landlock + user namespaces + seccomp. Direct reference implementation.
- `nix` crate: Full syscall access (`unshare`, `mount`, `prctl`, `setns`, capabilities).
- `seccompiler` or `libseccomp` bindings: For optional syscall filtering profiles.

### macOS Seatbelt Patterns
- Generate policy text and invoke `/usr/bin/sandbox-exec -f policy.sb` or `-p 'policy string'`.
- Real-world usage in AI coding agents (e.g., `codex-rs` style `spawn_command_under_seatbelt` that builds writable roots and network toggles).
- Policy language examples widely documented (Hacktricks, etc.). `(deny default)`, `(allow file-read* (subpath ...))`, etc.

### Networking (R5)
- **pasta** (from `passt` project): Modern rootless userspace networking. Excellent for N2. Integrates with network namespaces. Used in Podman and referenced in Rust sandbox experiments (Zerobox, redoubtful).
- `slirp4netns` as fallback.
- For proxy: `hyper` + custom middleware, or external `mitmproxy` with generated config for rapid MVP.

### Windows (Future)
- `win32job-rs`: Safe Job Objects API.
- `rappct`: AppContainer support.
- Historical: Trail of Bits `AppJailLauncher-rs` patterns for AppContainer + token manipulation.

### Related / Inspirational Projects
- **Zerobox** (Rust single-binary sandbox CLI using sandboxing crates + pasta networking).
- **redoubtful** (WIP Linux agent sandbox in Rust using bwrap + pasta + shadow home + modular profiles).
- Various Reddit discussions and guides on Landlock + seccomp for AI/LLM tool sandboxes (2025–2026).
- WASM-based alternatives (e.g., `agent-sandbox`) exist but are complementary/in-process rather than OS-level process wrappers.

These building blocks mean the core path features (R2 + R4) can be delivered with relatively low reinvention.

---

## 7. Proposed Rust Tool Architecture

**Binary name**: `isol8` (subcommand `run`). *(Primary inspiration: the Agent Safehouse project.)*

**High-level crates / modules**:
- `cli/` — clap definitions, argument parsing, profile loading.
- `profile/` — `Profile`, `ProfileLayer`, merging logic, TOML (de)serialization. Example layer:
  ```toml
  [profile.base]
  paths = [
    { path = "/usr", access = "ro" },
    { path = "/tmp", access = "rw" },
  ]
  env = { PATH = "/usr/bin:/bin" }
  home_replace = { enabled = true, auto_scratch = true, seed = ["~/.gitconfig"] }
  network = { tier = "n1", allow_domains = ["github.com", "*.githubusercontent.com"] }

  [profile.rust]
  paths = [ { path = "~/.cargo", access = "rw" } ]
  ```
- `backends/`:
  - `linux.rs` — Landlock ruleset builder + optional ns setup + `pasta` orchestration.
  - `macos.rs` — Seatbelt policy string generator + `Command` setup.
  - `windows.rs` — stub for later.
- `env.rs` — minimal environment construction (HOME first).
- `net/` (future) — proxy config generation, N2/N3 helpers.
- `spawn.rs` — cross-platform child execution with policy application.

**Key invariants**:
- Effective `$HOME` resolved and applied **before** any path grant computation.
- Deny-by-default everywhere.
- Single binary, no persistent services.
- Clear "effective policy" reporting (`--dry-run` or verbose mode).

**Error handling & UX**: Helpful messages when userns disabled, Landlock unavailable, `pasta` missing, etc. Suggest fixes (sysctl, install package).

---

## 8. Implementation Roadmap & Future Steps

### Phase 1: Core Path + HOME (Primary Focus) — Linux + macOS MVP
- Profile parser + merger.
- Linux: Landlock rules for ro/rw + basic HOME replace (env + rules).
- macOS: Seatbelt policy generator for paths + HOME.
- Minimal env sanitization.
- Basic CLI: `isol8 run --profile rust --add-dirs-rw /my/project cargo build`.
- Auto scratch home with optional seeding.
- Testing on common toolchains (git, cargo, node, python, etc.).

### Phase 2: Polish & Cross-Platform Basics
- Full R3 env features (`--env-pass`, `--env-file`).
- Resource limits (R1.3).
- `--dry-run` / effective policy dump.
- WSL2 testing & interop hardening.
- Documentation + examples.

### Phase 3: Network Tiers (Secondary Focus — Detailed)
- **N1**: Profile-driven domain allowlist + proxy env vars. Optional simple Rust filtering proxy (hostname/SNI first).
- **N2 (Linux)**:
  1. Detect `pasta` binary.
  2. Enter network namespace (`nix::sched::unshare(CLONE_NEWNET)`).
  3. Spawn `pasta` configured to forward only to the local proxy (or host for N0).
  4. Exec target inside the ns (or use `setns`).
- **N3**:
  - Separate small `isol8-net-helper` binary.
  - `setcap cap_net_admin+ep` during install.
  - Helper creates netns + veth, configures nftables `tproxy` redirect to proxy, drops caps, execs main process.
- DNS control, IPv6 handling, MITM support (with per-toolchain CA injection and pinning exemptions).
- Graceful fallback + capability probing.
- Profile network layers (e.g., `github` layer adds domains).

### Phase 4: Further Refinements & Advanced Features
- **Seccomp profiles**: Predefined safe sets (e.g., "cli-tool", "build-system") + custom. Integrate with Landlock.
- **Observability**: Structured JSON logs for policy decisions, denied accesses (great for debugging agent behavior). Optional audit mode.
- **Testing harness**: Integration tests that run real commands (git, cargo, curl with restricted net) and assert allowed/denied behavior. Fuzz profile merging.
- **Performance & overhead**: Benchmarks (Landlock is near-zero; ns has measurable but acceptable cost). Optimize hot paths.
- **Security hardening**: TOCTOU mitigations, fd leak prevention, comprehensive capability dropping, static analysis + security audit checklist.
- **Hybrid isolation modes**: Pure Landlock (fastest, simplest) vs full ns (strongest isolation + perfect HOME) — user or profile selectable.
- **GUI / Desktop app support** (stretch): More complex on all platforms (especially macOS entitlements, Windows AppContainer GUI quirks).
- **WSL2 specifics**: Auto `wsl.conf` advice, path translation helpers, interop disable.
- **Extensibility**: Allow external profile layers (plugins?) or custom backends.
- **Packaging**: Single static binary (musl or cross-compiled), Homebrew formula, cargo install, Linux distro packages.
- **Documentation generation**: Export effective grants as human-readable or machine-readable (JSON) for agent frameworks.
- **Hybrid with WASM**: Optional in-process WASM sandbox layer for extra safety on interpreted languages/tools.

### Phase 5: Windows Backend (Later)
- AppContainer creation + capability grants.
- Job Objects for resource limits.
- WFP for network (N2/N3-ish).
- HOME replacement via multiple env vars + registry considerations (best-effort).
- Profile rendering to ACLs + caps (more verbose than text policies).

### Long-term / Future Ideas
- Kernel-level eBPF integration for even finer observability/enforcement (optional, privileged).
- Integration with container runtimes (Podman/Docker rootless profiles generated from isol8 profiles).
- WebAssembly Component Model or WASI preview for language-agnostic tool sandboxing.
- Formal verification or model checking of profile merging logic.
- Community profile repository (curated safe layers for popular toolchains and AI agent frameworks).

---

## 9. Getting Started Recommendations for Implementation

1. Start with **Linux Landlock path + HOME MVP** using the `landlock` crate + `nix`.
2. Add macOS Seatbelt backend early (high value, relatively easy once policy generator exists).
3. Define the profile TOML schema and merger first — it drives everything.
4. Use existing projects (`sandbox-rs`, `redoubtful`, Zerobox ideas) as references rather than copying code.
5. Prioritize excellent error messages and `--dry-run` / verbose policy output — critical for developer/agent trust.
6. Test aggressively with real-world AI coding agent workloads (file ops, git, package managers, network calls to allowed domains).

This specification provides a complete blueprint. The Rust ecosystem in 2026 already contains most of the low-level pieces needed for a high-quality implementation focused on path control and HOME replacement, with clear extension points for networking and refinements.

**Next action**: Prototype the profile struct + Linux Landlock renderer. The rest follows naturally.

---

*Document compiled from original requirements + extensive Rust ecosystem research (June 2026). All original requirements preserved and augmented with implementation guidance.*
