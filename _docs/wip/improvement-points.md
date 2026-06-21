# isol8 — improvement feedback

Notes from building this setup with isol8 0.10.x. Ordered roughly by impact.
Overall: the layered profile model + `--home` replacement + builtin toolchain
profiles are excellent and made most of a hand-rolled setup unnecessary. The
friction below is mostly about persistent homes, observability, and ergonomics.

## 1. Seeding collides with a persistent `--home` — ✅ done
*Implemented: seeding is now first-creation-only (skip-if-exists), so a persistent
home keeps its first snapshot and a re-run never fails overwriting the read-only seed.*

The `base` profile seeds `~/.gitconfig` into the home **read-only (444)** at
launch. With `auto_scratch` that's fine (fresh home each run), but with a fixed
persistent `--home` the second launch fails:

```
Error: seeding /Users/<you>/.gitconfig -> <home>/.gitconfig
Caused by: Permission denied (os error 13)
```

The seeded file from the previous run can't be overwritten. Worked around by
`rm -f`-ing the seed target in the launcher before each run.
**Suggestions:** seed should be idempotent — skip-if-exists, or `chmod +w`
before re-copy, or only seed on first creation of a persistent home. Also: don't
force `0444`, or make the perms configurable.

## 2. `home_replace` / `seed` can't be overridden by a child profile — ✅ done (flag)
*Implemented: `--no-seed` clears all seed entries for the run. Array merge semantics
unchanged (kept simple); the flag is the escape hatch.*

Setting `[home_replace] seed = []` in a profile that `requires = ["base"]` did
**not** suppress base's seed (arrays appear to merge additively / base wins).
There's no obvious way to opt a persistent home out of seeding.
**Suggestion:** a CLI flag (`--no-seed`) and/or last-layer-wins override for
`home_replace`.

## 3. `--show-policies` layer stack hides required/auto layers — ✅ done
*Implemented: the layer stack now prints the fully resolved (deps-first) layers, each
tagged `(explicit)` / `(auto)` / `(required)`.*

`== layer stack ==` prints only the explicitly named profile (e.g. `dev-home`),
even though `requires` and `--auto-profiles` layers are merged into the grants.
This made it look like nothing loaded until I inspected the full grant list.
**Suggestion:** print the *fully resolved* layer stack (with provenance:
explicit / required-by / auto), matching what actually contributes grants.

## 4. No CLI env passthrough/override — ✅ done
*Implemented: `--env-pass NAME` (pull a host var through) and `--set-env K=V` (explicit,
highest precedence). Both compose over profile `[env]`.*

Env can only be set via a profile `[env]` block. There's no
`--env-pass NAME` / `--set-env K=V` like some sandboxes have, so adapting env per
run means editing a TOML.
**Suggestion:** add `--env-pass`/`--set-env` flags (compose over profile env).

## 5. cwd isn't auto-granted
The working directory must be granted explicitly (`--add-dirs-rw "$PWD"`).
Forgetting it yields a confusing "can't read my own project" situation.
**Suggestion:** an opt-in `--grant-cwd` (or `--workdir`) flag, or auto-grant the
cwd RW when no `--add-dirs*` is given.

## 6. `--profile-path` warning every run — ✅ done
*Removed: the per-run warning is gone (it printed on every launch and was just noise).*

`warning: --profile-path may load raw Seatbelt rules; only use profiles from
trusted sources` prints on every launch. Reasonable once, noisy forever.
**Suggestion:** a trust mechanism (config entry / `--trust-profile-path`) or
checksum-pinning to silence it for known-local profiles.

## 7. Nested-sandbox failure is silent (SIGABRT 134)
Running isol8 inside an already-sandboxed process aborts the child with exit 134
and **no diagnostic** — hard to distinguish from a policy bug.
**Suggestion:** detect that `sandbox-exec` can't nest (or that a profile is
already applied) and print a clear, actionable error.

## 8. Toolchain profiles assume standard install locations
`toolchains/rust` grants `~/.cargo`/`~/.rustup`; a non-standard install
(e.g. rust under `~/works/rust`) needs a custom profile with absolute paths +
env. That's fine, but…
**Suggestion:** allow toolchain profiles to read root locations from env
(`CARGO_HOME`, `GOROOT`, …) so they cover non-standard installs automatically.

## 9. Profiles can't reference the real home — ✅ done
*Implemented: the `#HOME` token expands to the real home (before `~` expansion), so a
grant like `#HOME/.ssh` survives an active `--home`. Works in grants and `--add-dirs-*`.*

Profile `~` resolves to the *replacement* home, so there's no portable way to
grant something in the real home — you must hardcode absolute `/Users/<you>/…`
paths, which makes profiles machine-specific and non-shareable.
**Suggestion:** a token for the real home (e.g. `%REAL_HOME%` or
`${HOST_HOME}`) usable in profile paths.

## 10. Error messages lack layer context
Failures (like #1) don't say which layer/key caused them.
**Suggestion:** attribute errors to the originating profile + setting.

## Smaller notes
- `@profiles-show <name>` doesn't show the transitively-required layers; a
  `--resolved` view would help.
- `--auto-profiles` silently no-ops when the command's executable isn't found on
  PATH; a note ("no agent profile matched 'codex'") would aid debugging.
- A `@doctor` command to validate a profile + print effective grants for a given
  command in one shot would be a nice ergonomic win.

## What's great (keep it)
- Deny-by-default + composable profile layers with `requires`.
- Builtin agent + toolchain + integration profiles — huge head start.
- Native `--home` replacement; caches/keychain redirect into it cleanly.
- `--show-policies` emitting the actual SBPL — auditable and trustworthy.
