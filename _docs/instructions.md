# isol8 — Usage Instructions

How to build and run `isol8`. Reflects what is **implemented today**: the macOS
Seatbelt backend plus the path / HOME / env pipeline (Phase 1). Linux and the
network tiers are not wired yet — see [`AGENTS.md`](../AGENTS.md) "Current status".

> **Platform:** enforcement works on **macOS 12+** (via `/usr/bin/sandbox-exec`).
> On other OSes the command parses and `--dry-run` works, but `run` will error
> because no enforcing backend exists yet.

---

## 1. Build

```sh
cargo build                      # debug binary at target/debug/isol8
just build                       # same, via the justfile
```

The binary is `isol8`. A second binary, `isol8-field-test`, runs the real-sandbox
field tests (see §6).

---

## 2. Command shape

```
isol8 run [OPTIONS] <CMD>...
```

Everything after the options is the command to confine. Use `--` to separate
isol8's flags from the target command's flags:

```sh
isol8 run --profile macos-system -- /bin/sh -c 'echo hello'
```

### Options

| Flag | Repeatable | Meaning |
|------|:---------:|---------|
| `--profile <NAME>` | yes | Enable a named profile layer. Layers merge deny-first; `requires` deps are pulled in automatically. |
| `--add-dirs-rw <PATH>` | yes | Grant read-write access to a path (highest-priority override layer). |
| `--add-dirs-ro <PATH>` | yes | Grant read-only access to a path. |
| `--home <PATH>` | no | Use `<PATH>` as the confined `$HOME`. Defaults to an auto scratch home when a profile enables HOME replacement. |
| `--dry-run` | no | Print the effective policy (grants, env, command, generated SBPL) and exit without running. |
| `-h, --help` | no | Print help. |

---

## 3. Built-in profiles

Embedded in the binary (authored in `profiles/`):

- **`base`** — cross-platform minimum: ro `/usr` + `/bin`, rw `/tmp`, a minimal
  `PATH`, and an auto-scratch `$HOME` seeded read-only with `~/.gitconfig`.
- **`macos-system`** — `requires = ["base"]`; adds the macOS runtime essentials a
  command needs to start under `(deny default)` (ro `/System`, the mandatory
  `literal "/"`, `/private/var/select`, and the `process-exec`/`process-fork`
  capabilities). **This is the layer to start from on macOS.**

User profiles can be dropped into `$XDG_CONFIG_HOME/isol8/profiles/` (or
`~/.config/isol8/profiles/`) as `<name>.toml`; they're selectable by `--profile`.
See [`profile-model.md`](./profile-model.md) for the full schema.

---

## 4. Examples

```sh
# Inspect the effective policy without running anything (works on any OS):
isol8 run --profile macos-system --dry-run -- echo hi

# Run a trivial command confined (macOS):
isol8 run --profile macos-system -- /bin/sh -c true

# Confine a command and give it read-write access to one project directory:
isol8 run --profile macos-system --add-dirs-rw "$PWD" -- /bin/sh -c 'echo built > out.txt'

# Read-only access to a reference dir, plus a project workdir:
isol8 run --profile macos-system \
  --add-dirs-ro /opt/reference \
  --add-dirs-rw "$PWD" \
  -- some-tool

# Use an explicit replacement HOME instead of an auto scratch dir:
isol8 run --profile macos-system --home /tmp/agent-home -- /bin/sh -c 'echo $HOME'
```

Via `just`:

```sh
just run run --profile macos-system --dry-run -- echo hi
```

---

## 5. What confinement does

- **Filesystem** — deny-by-default. Only the merged profile's grants are reachable;
  everything else returns `Operation not permitted`. `--add-dirs-rw`/`-ro` win over
  profile layers.
- **HOME** — replaced first, before any path grant is computed. `~`-relative grants
  target the replacement home, and the real home is **not** granted unless re-added.
- **Environment** — sanitized to an allowlist (`HOME`, `PATH`, `SHELL`, `TMPDIR`,
  `USER`, `LOGNAME`, `PWD`); host secrets like `SECRET_TOKEN` do not pass through.
  Profile `env` defaults are folded in without overriding allowlisted host values.

---

## 6. Tests

```sh
just test            # unit + integration tests (cargo test) — all platforms
just field-test      # real-sandbox field tests on this machine (macOS)
just ci              # full gate: fmt-check + clippy -D warnings + build + test
```

Field tests build ad-hoc profiles + scratch dirs under the temp dir, run probes
through the **real** sandbox, and assert the OS actually enforced the policy
(deny outside grants, allow rw workspace, deny writes to ro, scratch HOME, env
sanitization). Pass `--keep` to retain the temp workspace for inspection:

```sh
just field-test --keep
```

---

## 7. Troubleshooting

- **`getcwd: Operation not permitted` noise** — the confined process inherits
  isol8's working directory, which isn't granted automatically yet. Add it with
  `--add-dirs-rw "$PWD"` (or `cd /` first). Auto-cwd-grant is a planned follow-up.
- **`git` / `cargo` fail to start on macOS** — the system `git`/`cargo` are
  xcode-select shims that need developer-tool paths beyond `macos-system`. Grant
  the needed paths with `--add-dirs-ro` or add a toolchain profile layer.
- **`macos backend not yet implemented` / errors on Linux** — only macOS has an
  enforcing backend today. Use `--dry-run` elsewhere to inspect the policy.
- **Policy failed to compile** — `--dry-run` prints the generated SBPL; that's the
  fastest way to see what was emitted and why `sandbox-exec` rejected it.
