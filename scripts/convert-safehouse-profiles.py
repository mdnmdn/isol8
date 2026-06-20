#!/usr/bin/env python3
"""Convert agent-safehouse .sb profiles to isol8 TOML layers."""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterable

SAFEHOUSE_ROOT = Path("/tmp/agent-safehouse/profiles")
OUT_ROOT = Path(__file__).resolve().parent.parent / "profiles"

DIR_MAP = {
    "50-integrations-core": "integrations",
    "55-integrations-optional": "integrations",
    "30-toolchains": "toolchains",
    "40-shared": "shared",
    "60-agents": "agents",
    "65-apps": "apps",
}

TOP_LEVEL_MAP = {
    "00-base.sb": "base",
    "10-system-runtime.sb": "macos/system-runtime",
    "20-network.sb": "network",
}

# YAML + $$require$$ metadata from sample-profiles-safehouse.yaml and .sb headers.
REQUIRES: dict[str, list[str]] = {
    "macos/system-runtime": ["base"],
    "network": ["base", "macos/system-runtime"],
    # toolchains
    "toolchains/apple-toolchain-core": ["macos/system-runtime"],
    "toolchains/bun": ["macos/system-runtime"],
    "toolchains/deno": ["macos/system-runtime"],
    "toolchains/elixir": ["macos/system-runtime"],
    "toolchains/go": ["macos/system-runtime"],
    "toolchains/java": ["macos/system-runtime"],
    "toolchains/node": ["macos/system-runtime"],
    "toolchains/perl": ["macos/system-runtime"],
    "toolchains/php": ["macos/system-runtime"],
    "toolchains/python": ["macos/system-runtime"],
    "toolchains/ruby": ["macos/system-runtime"],
    "toolchains/runtime-managers": ["macos/system-runtime"],
    "toolchains/rust": ["macos/system-runtime"],
    # shared
    "shared/agent-common": ["macos/system-runtime"],
    "shared/ipc-sysv-sem": ["macos/system-runtime"],
    # integrations core
    "integrations/container-runtime-default-deny": ["macos/system-runtime"],
    "integrations/git": ["macos/system-runtime", "shared/agent-common"],
    "integrations/launch-services": ["macos/system-runtime"],
    "integrations/scm-clis": ["macos/system-runtime", "integrations/git"],
    "integrations/ssh-agent-default-deny": ["macos/system-runtime"],
    "integrations/worktree-common-dir": ["integrations/git"],
    "integrations/worktrees": ["integrations/git"],
    # integrations optional
    "integrations/1password": ["macos/system-runtime"],
    "integrations/agent-browser": ["macos/system-runtime", "network", "integrations/chromium-full"],
    "integrations/browser-native-messaging": ["macos/system-runtime"],
    "integrations/chromium-full": ["integrations/chromium-headless"],
    "integrations/chromium-headless": ["macos/system-runtime", "network"],
    "integrations/cleanshot": ["integrations/macos-gui"],
    "integrations/clipboard": ["macos/system-runtime"],
    "integrations/cloud-credentials": ["macos/system-runtime"],
    "integrations/cloud-storage": ["macos/system-runtime", "network"],
    "integrations/docker": ["macos/system-runtime"],
    "integrations/electron": ["integrations/macos-gui"],
    "integrations/keychain": ["macos/system-runtime"],
    "integrations/kubectl": ["macos/system-runtime"],
    "integrations/lldb": ["integrations/process-control"],
    "integrations/macos-gui": ["integrations/clipboard"],
    "integrations/microphone": ["macos/system-runtime"],
    "integrations/playwright-chrome": ["integrations/chromium-full", "integrations/macos-gui"],
    "integrations/process-control": ["macos/system-runtime"],
    "integrations/shell-init": ["macos/system-runtime"],
    "integrations/spotlight": ["macos/system-runtime"],
    "integrations/ssh": ["macos/system-runtime"],
    "integrations/vscode": ["apps/vscode-app"],
    "integrations/xcode": ["toolchains/apple-toolchain-core", "integrations/macos-gui"],
    # agents
    "agents/aider": ["macos/system-runtime", "integrations/git", "toolchains/python"],
    "agents/amp": ["macos/system-runtime", "integrations/clipboard"],
    "agents/auggie": ["macos/system-runtime"],
    "agents/claude-code": [
        "integrations/keychain",
        "integrations/browser-native-messaging",
        "integrations/microphone",
    ],
    "agents/cline": ["macos/system-runtime", "integrations/keychain"],
    "agents/codex": ["macos/system-runtime", "integrations/keychain"],
    "agents/copilot-cli": ["macos/system-runtime", "integrations/keychain"],
    "agents/cursor-agent": ["macos/system-runtime", "integrations/vscode"],
    "agents/droid": ["macos/system-runtime"],
    "agents/gemini": ["macos/system-runtime", "integrations/keychain"],
    "agents/goose": ["macos/system-runtime", "integrations/keychain"],
    "agents/kilo-code": ["macos/system-runtime", "integrations/keychain"],
    "agents/opencode": ["macos/system-runtime"],
    "agents/pi": ["macos/system-runtime"],
    # apps
    "apps/claude-app": ["integrations/electron", "agents/claude-code"],
    "apps/codex-app": ["integrations/keychain", "integrations/electron"],
    "apps/vscode-app": ["integrations/macos-gui", "integrations/vscode"],
}

FILE_OPS = {
    "file-read*",
    "file-write*",
    "file-read-metadata",
    "file-read-xattr",
    "file-write-xattr",
}

UNRESTRICTED_CAPS = {
    "process-exec": "process-exec",
    "process-fork": "process-fork",
    "sysctl-read": "sysctl-read",
    "pseudo-tty": "pseudo-tty",
    "ipc-sysv-sem": "sysv-sem",
    "process-info*": "process-info",
    "signal": "signal",
    "mach-priv-task-port": None,  # always raw (target same-sandbox)
}

ACCESS_MAP = {
    ("allow", frozenset({"file-read*"})): "ro",
    ("allow", frozenset({"file-read-metadata"})): "metadata",
    ("allow", frozenset({"file-read*", "file-write*"})): "rw",
    ("deny", frozenset({"file-read*", "file-write*"})): "none",
}


@dataclass
class PathGrant:
    path: str
    access: str
    match: str = "subpath"


@dataclass
class ParsedProfile:
    description: str = ""
    requires: list[str] = field(default_factory=list)
    paths: list[PathGrant] = field(default_factory=list)
    capabilities: list[str] = field(default_factory=list)
    raw_rules: list[str] = field(default_factory=list)
    issues: list[str] = field(default_factory=list)


def sb_ref_to_layer(ref: str) -> str:
    ref = ref.strip()
    if ref.endswith(".sb"):
        ref = ref[:-3]
    if "/" in ref:
        directory, name = ref.split("/", 1)
        mapped = DIR_MAP.get(directory, directory.split("-", 1)[-1] if directory[0].isdigit() else directory)
        return f"{mapped}/{name}"
    if ref == "00-base":
        return "base"
    if ref == "10-system-runtime":
        return "macos/system-runtime"
    if ref == "20-network":
        return "network"
    return ref


def layer_name_for_sb(relative: str) -> str | None:
    if relative in TOP_LEVEL_MAP:
        return TOP_LEVEL_MAP[relative]
    parts = relative.replace(".sb", "").split("/")
    if len(parts) != 2:
        return None
    directory, stem = parts
    mapped = DIR_MAP.get(directory)
    if not mapped:
        return None
    return f"{mapped}/{stem}"


def strip_comments(text: str) -> tuple[str, str, list[str]]:
    """Return (code, description, issues)."""
    desc_lines: list[str] = []
    code_lines: list[str] = []
    in_header = True
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith(";;"):
            content = stripped[2:].strip()
            if in_header and content and not content.startswith("-"):
                if content.startswith("Source:"):
                    in_header = False
                elif not content.startswith("$$") and not content.startswith("#safehouse"):
                    desc_lines.append(content)
            continue
        if stripped.startswith(";") and not stripped.startswith(";;"):
            # Commented-out SBPL form.
            continue
        if stripped:
            in_header = False
        code_lines.append(line)
    return "\n".join(code_lines), " ".join(desc_lines[:2]), []


def strip_form_comments(form: str) -> str:
    """Remove ;; line comments from an SBPL form without touching string literals."""
    out: list[str] = []
    i = 0
    n = len(form)
    in_str = False
    while i < n:
        if in_str:
            if form[i] == "\\" and i + 1 < n:
                out.append(form[i : i + 2])
                i += 2
                continue
            out.append(form[i])
            if form[i] == '"':
                in_str = False
            i += 1
            continue
        if form[i : i + 2] == "#\"":
            in_str = True
            out.append(form[i])
            i += 1
            continue
        if form[i] == '"':
            in_str = True
            out.append(form[i])
            i += 1
            continue
        if form[i : i + 2] == ";;":
            while i < n and form[i] != "\n":
                i += 1
            continue
        out.append(form[i])
        i += 1
    return "".join(out)


def parse_sbpl_atom_value(raw: str) -> str | None:
    raw = raw.strip()
    if raw.startswith('#"') and raw.endswith('"#'):
        return raw[2:-2]
    if raw.startswith('#"') and raw.endswith('"'):
        return raw[2:-1]
    if raw.startswith('"') and raw.endswith('"'):
        return raw[1:-1]
    if "HOME_DIR" in raw or raw.startswith("(string-append"):
        return None
    return raw


def parse_requires_from_sb(text: str, layer: str) -> list[str]:
    m = re.search(r"\$\$require=([^$]+)\$\$", text)
    if m:
        refs = [r.strip() for r in m.group(1).split(",")]
        return [sb_ref_to_layer(r) for r in refs if r]
    return REQUIRES.get(layer, ["macos/system-runtime"])


def expand_home(matcher: str, arg: str) -> tuple[str, str] | None:
    parsed = parse_sbpl_atom_value(arg)
    if parsed is None:
        return None
    if matcher == "home-subpath":
        return f"~{parsed}", "subpath"
    if matcher == "home-literal":
        return f"~{parsed}", "literal"
    if matcher == "home-prefix":
        return f"~{parsed}", "prefix"
    if matcher in ("subpath", "literal", "prefix"):
        return parsed, matcher
    if matcher == "regex":
        return parsed, "regex"
    return parsed, matcher


def tokenize_matcher_args(body: str) -> list[tuple[str, str]]:
    results: list[tuple[str, str]] = []
    i = 0
    n = len(body)
    while i < n:
        while i < n and body[i].isspace():
            i += 1
        if i >= n or body[i] != "(":
            i += 1
            continue
        depth = 0
        start = i
        while i < n:
            if body[i] == "(":
                depth += 1
            elif body[i] == ")":
                depth -= 1
                if depth == 0:
                    i += 1
                    break
            i += 1
        chunk = body[start:i].strip()
        inner = chunk[1:-1].strip()
        m = re.match(
            r"^(subpath|literal|prefix|regex|home-subpath|home-literal|home-prefix)\s+(.+)$",
            inner,
            re.DOTALL,
        )
        if not m:
            continue
        kind, arg_raw = m.group(1), m.group(2).strip()
        expanded = expand_home(kind, arg_raw)
        if expanded:
            results.append(expanded)
    return results


def extract_forms(code: str) -> list[str]:
    forms: list[str] = []
    i = 0
    n = len(code)
    while i < n:
        if code[i] != "(":
            i += 1
            continue
        depth = 0
        start = i
        while i < n:
            if code[i] == "(":
                depth += 1
            elif code[i] == ")":
                depth -= 1
                if depth == 0:
                    i += 1
                    forms.append(code[start:i].strip())
                    break
            i += 1
    return forms


def classify_form(form: str, parsed: ParsedProfile) -> None:
    if not form.startswith("("):
        return
    form = strip_form_comments(form)
    inner = form[1:-1].strip()
    parts = inner.split(None, 1)
    if not parts:
        return
    action = parts[0]
    rest = parts[1] if len(parts) > 1 else ""

    if action in ("define", "version", "deny") and rest == "default":
        return
    if action == "define":
        return

    if action not in ("allow", "deny"):
        parsed.raw_rules.append(form)
        return

    # Split operations from matchers at first path matcher.
    matcher_start = re.search(
        r"\((?:subpath|literal|prefix|regex|home-subpath|home-literal|home-prefix|global-name|global-name-regex|ipc-posix-name|preference-domain|iokit-user-client-class|remote|target|local)",
        rest,
    )
    if not matcher_start:
        ops = rest.split()
        op_set = set(ops)
        if op_set <= set(UNRESTRICTED_CAPS) and "target" not in rest:
            for op in ops:
                cap = UNRESTRICTED_CAPS.get(op)
                if cap and cap not in parsed.capabilities:
                    parsed.capabilities.append(cap)
                elif cap is None and op in UNRESTRICTED_CAPS:
                    parsed.raw_rules.append(form)
            if op_set & {"network*", "network-outbound", "network-inbound", "network-bind"}:
                parsed.issues.append(f"network rule deferred to raw/comments: {form[:80]}")
                parsed.raw_rules.append(form)
            elif not (op_set <= set(UNRESTRICTED_CAPS) or op_set & {"network*", "network-outbound"}):
                parsed.raw_rules.append(form)
        else:
            parsed.raw_rules.append(form)
        return

    op_part = rest[: matcher_start.start()].strip()
    matcher_part = rest[matcher_start.start() :]
    ops = frozenset(op_part.split())

    access = ACCESS_MAP.get((action, ops))
    if access and not any(
        m in matcher_part
        for m in (
            "global-name",
            "ipc-posix-name",
            "preference-domain",
            "iokit-user-client-class",
            "remote",
            "target",
        )
    ):
        if "file-read-xattr" in ops or "file-write-xattr" in ops:
            parsed.raw_rules.append(form)
            return
        grants = tokenize_matcher_args(matcher_part)
        # Only extract when every matcher atom in the form was understood; otherwise
        # keep the whole SBPL rule in raw (e.g. HOME_DIR regex denies).
        matcher_atoms = len(re.findall(r"\((?:subpath|literal|prefix|regex|home-)", matcher_part))
        if not grants or len(grants) != matcher_atoms:
            parsed.raw_rules.append(form)
            return
        for path, match in grants:
            parsed.paths.append(PathGrant(path=path, access=access, match=match))
        return

    parsed.raw_rules.append(form)


def parse_sb_file(path: Path, layer: str) -> ParsedProfile:
    text = path.read_text()
    code, description, _ = strip_comments(text)
    parsed = ParsedProfile(
        description=description,
        requires=parse_requires_from_sb(text, layer),
    )
    for form in extract_forms(code):
        classify_form(form, parsed)
    return parsed


def toml_escape(s: str) -> str:
    return s.replace("\\", "\\\\").replace('"', '\\"')


def render_toml(layer: str, parsed: ParsedProfile, *, skip_network_raw: bool = False) -> str:
    lines: list[str] = []
    lines.append(f"# {layer}")
    if parsed.description:
        lines.append(f"# {parsed.description}")
    lines.append("# filter = { os = [\"macos\"] }  # macOS-only layer (filter not parsed yet)")
    lines.append("")

    if parsed.requires:
        req = ", ".join(f'"{r}"' for r in parsed.requires)
        lines.append(f"requires = [{req}]")
        lines.append("")

    if parsed.paths:
        lines.append("paths = [")
        for g in parsed.paths:
            if g.match == "subpath":
                lines.append(f'  {{ path = "{toml_escape(g.path)}", access = "{g.access}" }},')
            else:
                lines.append(
                    f'  {{ path = "{toml_escape(g.path)}", access = "{g.access}", match = "{g.match}" }},'
                )
        lines.append("]")
        lines.append("")

    macos_lines: list[str] = []
    if parsed.capabilities:
        caps = ", ".join(f'"{c}"' for c in parsed.capabilities)
        macos_lines.append(f"capabilities = [{caps}]")

    raw = "\n".join(strip_form_comments(r) for r in parsed.raw_rules).strip()
    if skip_network_raw:
        raw = ""
    if raw:
        macos_lines.append(f'raw = """\n{raw}\n"""')

    if macos_lines:
        lines.append("[macos]")
        lines.extend(macos_lines)
        lines.append("")

    return "\n".join(lines).rstrip() + "\n"


def convert_all() -> tuple[list[str], list[str]]:
    created: list[str] = []
    issues: list[str] = []

    for sb in sorted(SAFEHOUSE_ROOT.rglob("*.sb")):
        relative = sb.relative_to(SAFEHOUSE_ROOT).as_posix()
        layer = layer_name_for_sb(relative)
        if layer is None:
            issues.append(f"skipped unmapped: {relative}")
            continue
        if layer == "base":
            issues.append("kept existing profiles/base.toml (not overwriting 00-base.sb)")
            continue

        parsed = parse_sb_file(sb, layer)
        skip_network = layer == "network"
        if skip_network:
            parsed.raw_rules = []
            parsed.capabilities = []
            parsed.paths = []
            issues.append("network layer: requires-only stub (network block not parsed yet)")

        out_path = OUT_ROOT / f"{layer}.toml"
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(render_toml(layer, parsed, skip_network_raw=skip_network))
        created.append(str(out_path.relative_to(OUT_ROOT.parent)))

        for issue in parsed.issues:
            issues.append(f"{layer}: {issue}")

    # Backward-compat alias
    alias = OUT_ROOT / "macos-system.toml"
    alias.write_text(
        '# macos-system — backward-compat alias for macos/system-runtime.\n'
        'requires = ["macos/system-runtime"]\n'
    )
    created.append(str(alias.relative_to(OUT_ROOT.parent)))

    return created, issues


def main() -> int:
    if not SAFEHOUSE_ROOT.is_dir():
        print(f"safehouse profiles not found at {SAFEHOUSE_ROOT}", file=sys.stderr)
        return 1
    created, issues = convert_all()
    print("Created/updated:")
    for f in sorted(created):
        print(f"  {f}")
    if issues:
        print("\nConversion notes:")
        for i in issues:
            print(f"  - {i}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())