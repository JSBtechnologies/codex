#!/usr/bin/env bash
# Migrate a previous Codex home into the one this fork uses.
#
# The fork reads the same $CODEX_HOME (default ~/.codex) and the same
# config.toml as upstream, so on one machine nothing needs migrating: install
# the fork and it picks up your config, login and trusted folders as they are.
# This script is for the two cases where that is not enough:
#
#   1. A separate home for the fork (CODEX_HOME=~/.codex-fork), kept apart
#      from an upstream install.
#   2. Moving a Windows Codex setup into WSL (or any other machine), where the
#      trusted-folder keys are Windows paths that mean nothing on Linux.
#
# What it carries over (config, never machine state):
#   config.toml   translated and merged (see below)
#   auth.json     your login (ChatGPT tokens / API key); copied with mode 600
#   AGENTS.md, hooks.json, rules/, skills/ (your own, not the bundled .system
#   set), memories/, agents/
#   --with-sessions adds sessions/, archived_sessions/, history.jsonl and
#   session_index.jsonl
#
# What it leaves behind on purpose: packages/ (install the fork with
# install-fork.sh instead), the *.sqlite state, cache/, log/, tmp/, .tmp/,
# .sandbox/, plugins/cache, version.json and installation_id.
#
# config.toml handling:
#   - [projects."C:\\x\\y"] / [projects.'c:\x\y'] trust entries become
#     [projects."/mnt/c/x/y"]; \\wsl.localhost\<distro>\p and \\wsl$\<distro>\p
#     become /p. Windows Codex stores these keys lowercased, while the TUI's
#     "Trust this folder?" check compares the key with the real cwd spelling,
#     so the on-disk capitalisation is recovered for folders that exist here
#     (/mnt/c/users/me -> /mnt/c/Users/me).
#   - If the destination has no config.toml the translated file is written.
#   - If it has one, the migration is additive: top-level keys the destination
#     lacks are appended, and inside projects / mcp_servers / profiles /
#     model_providers / permissions, missing entries are appended. Keys that
#     already exist are never changed; they are listed so you can merge by
#     hand. --force replaces the file instead (after backing it up).
#   - Windows-only settings ([windows], windows_wsl_setup_acknowledged) are
#     not carried into a Linux home.
#
# Usage:
#   scripts/install/migrate-codex-home.sh [--from DIR] [--to DIR]
#                                         [--with-sessions] [--force] [--dry-run]
#
#   --from DIR   the previous home. Default under WSL: the single
#                /mnt/<drive>/Users/<user>/.codex that exists; otherwise required.
#   --to DIR     the fork's home. Default: $CODEX_HOME, else ~/.codex.
#
# Needs bash and python3 3.11+ (tomllib). Nothing is written with --dry-run.
set -eu

say()  { printf '%s\n' "$*"; }
note() { printf 'migrate-codex-home: %s\n' "$*"; }
fail() { printf 'migrate-codex-home: %s\n' "$*" >&2; exit 1; }

SRC=""
DST="${CODEX_HOME:-$HOME/.codex}"
WITH_SESSIONS=0
FORCE=0
DRY_RUN=0

while [ $# -gt 0 ]; do
  case "$1" in
    --from) [ $# -ge 2 ] || fail "--from needs a directory"; SRC="$2"; shift 2 ;;
    --from=*) SRC="${1#--from=}"; shift ;;
    --to) [ $# -ge 2 ] || fail "--to needs a directory"; DST="$2"; shift 2 ;;
    --to=*) DST="${1#--to=}"; shift ;;
    --with-sessions) WITH_SESSIONS=1; shift ;;
    --force) FORCE=1; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) sed -n '2,50p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) fail "unknown argument: $1 (see --help)" ;;
  esac
done

command -v python3 >/dev/null 2>&1 || fail "python3 is required"
python3 -c 'import tomllib' 2>/dev/null || fail "python3 3.11+ (with tomllib) is required"

is_wsl=0
if [ -r /proc/version ] && grep -qi microsoft /proc/version; then is_wsl=1; fi

if [ -z "$SRC" ]; then
  if [ "$is_wsl" = 1 ]; then
    candidates=()
    for d in /mnt/[a-z]/Users/*/.codex; do
      [ -f "$d/config.toml" ] || [ -f "$d/auth.json" ] || continue
      candidates+=("$d")
    done
    if [ "${#candidates[@]}" -eq 1 ]; then
      SRC="${candidates[0]}"
      note "using the Windows Codex home ${SRC}"
    elif [ "${#candidates[@]}" -gt 1 ]; then
      fail "several Windows Codex homes found; pass --from: ${candidates[*]}"
    fi
  fi
  [ -n "$SRC" ] || fail "no source found; pass --from <previous codex home>"
fi

[ -d "$SRC" ] || fail "source $SRC is not a directory"
{ [ -f "$SRC/config.toml" ] || [ -f "$SRC/auth.json" ]; } \
  || fail "$SRC has neither config.toml nor auth.json; is it a Codex home?"

src_abs="$(cd "$SRC" && pwd -P)"
mkdir_dst() { [ "$DRY_RUN" = 1 ] || mkdir -p "$DST"; }
if [ -d "$DST" ]; then
  dst_abs="$(cd "$DST" && pwd -P)"
  [ "$src_abs" != "$dst_abs" ] || fail "source and destination are the same directory ($dst_abs); nothing to migrate"
fi

say "source:      $SRC"
say "destination: $DST"
[ "$DRY_RUN" = 1 ] && say "mode:        dry run (nothing is written)"
[ "$FORCE" = 1 ] && say "mode:        force (existing files are backed up and replaced)"
say ""

stamp="$(date +%Y%m%d-%H%M%S)"
backup() {
  # backup PATH: move an existing destination file aside before replacing it.
  [ -e "$1" ] || return 0
  note "backing up $1 -> $1.bak-$stamp"
  [ "$DRY_RUN" = 1 ] || mv "$1" "$1.bak-$stamp"
}

copy_file() {
  # copy_file NAME [MODE]
  local name="$1" mode="${2:-}"
  [ -f "$SRC/$name" ] || return 0
  if [ -e "$DST/$name" ] && [ "$FORCE" != 1 ]; then
    if cmp -s "$SRC/$name" "$DST/$name"; then
      say "  = $name (identical, kept)"
    else
      say "  ! $name exists in the destination and differs; kept (use --force to replace)"
    fi
    return 0
  fi
  say "  + $name"
  [ "$DRY_RUN" = 1 ] && return 0
  mkdir_dst
  backup "$DST/$name"
  cp -p "$SRC/$name" "$DST/$name"
  [ -n "$mode" ] && chmod "$mode" "$DST/$name"
  return 0
}

copy_dir() {
  # copy_dir NAME: add entries the destination lacks; --force also overwrites.
  local name="$1"
  [ -d "$SRC/$name" ] || return 0
  local count
  count="$(find "$SRC/$name" -mindepth 1 -not -path "$SRC/$name/.system" -not -path "$SRC/$name/.system/*" | wc -l)"
  if [ "$count" = 0 ]; then
    say "  - $name/ (nothing of yours in it)"
    return 0
  fi
  say "  + $name/ ($count entries; existing files are $([ "$FORCE" = 1 ] && echo replaced || echo kept))"
  [ "$DRY_RUN" = 1 ] && return 0
  mkdir_dst
  mkdir -p "$DST/$name"
  (cd "$SRC/$name" && find . -mindepth 1 -maxdepth 1 -not -name .system -print0) \
    | while IFS= read -r -d '' entry; do
        if [ "$FORCE" = 1 ]; then
          cp -a "$SRC/$name/$entry" "$DST/$name/"
        else
          cp -an "$SRC/$name/$entry" "$DST/$name/"
        fi
      done
}

say "files:"
copy_file auth.json 600
copy_file AGENTS.md
copy_file hooks.json
copy_dir rules
copy_dir skills
copy_dir memories
copy_dir agents
if [ "$WITH_SESSIONS" = 1 ]; then
  copy_dir sessions
  copy_dir archived_sessions
  copy_file history.jsonl 600
  copy_file session_index.jsonl
fi
say ""

if [ -f "$SRC/config.toml" ]; then
  say "config.toml:"
  [ "$DRY_RUN" = 1 ] || mkdir_dst
  if [ "$FORCE" = 1 ] && [ -f "$DST/config.toml" ]; then
    backup "$DST/config.toml"
  fi
  MIGRATE_SRC="$SRC" MIGRATE_DST="$DST" MIGRATE_DRY_RUN="$DRY_RUN" \
  MIGRATE_FORCE="$FORCE" MIGRATE_IS_WSL="$is_wsl" MIGRATE_STAMP="$stamp" \
  python3 - <<'PY'
import json, os, re, sys, tomllib

src_home = os.environ["MIGRATE_SRC"]
dst_home = os.environ["MIGRATE_DST"]
dry_run = os.environ["MIGRATE_DRY_RUN"] == "1"
force = os.environ["MIGRATE_FORCE"] == "1"
is_wsl = os.environ["MIGRATE_IS_WSL"] == "1"
stamp = os.environ["MIGRATE_STAMP"]
src_path = os.path.join(src_home, "config.toml")
dst_path = os.path.join(dst_home, "config.toml")

text = open(src_path, encoding="utf-8").read()
try:
    tomllib.loads(text)
except tomllib.TOMLDecodeError as e:
    sys.exit(f"migrate-codex-home: {src_path} is not valid TOML: {e}")

# ---- 1. translate Windows project keys -------------------------------------
DRIVE = re.compile(r"^([A-Za-z]):[\\/](.*)$")
UNC_WSL = re.compile(r"^\\\\(?:wsl\$|wsl\.localhost)\\([^\\]+)\\(.*)$", re.IGNORECASE)

def resolve_case(p: str) -> str:
    # Windows Codex stores trust keys lowercased, but the TUI's trust lookup
    # compares the configured key with the real cwd spelling, so recover the
    # on-disk capitalisation (/mnt/c/users/x -> /mnt/c/Users/x) where the
    # folder exists. Components that cannot be found are kept as written.
    parts = [x for x in p.split("/") if x]
    cur = "/"
    out = []
    resolved = True
    for part in parts:
        # An exists() check is useless on a case-insensitive drvfs mount, so
        # always take the spelling from the parent's directory listing.
        if resolved:
            try:
                entries = os.listdir(cur)
            except OSError:
                entries = []
            if part in entries:
                pass
            else:
                matches = [e for e in entries if e.lower() == part.lower()]
                if len(matches) == 1:
                    part = matches[0]
                else:
                    resolved = False
        out.append(part)
        cur = os.path.join(cur, part)
    return "/" + "/".join(out)

def translate_path(p: str):
    m = DRIVE.match(p)
    if m:
        new = "/mnt/%s/%s" % (m.group(1).lower(), m.group(2).replace("\\", "/"))
    else:
        m = UNC_WSL.match(p)
        if not m:
            return None
        new = "/" + m.group(2).replace("\\", "/")
    new = new.rstrip("/") or "/"
    fixed = resolve_case(new)
    if not os.path.isdir(fixed):
        print(f"  ! {fixed} does not exist here; the trust entry is kept with that spelling")
    return fixed

def decode_key(tok: str) -> str:
    if tok.startswith("'"):
        return tok[1:-1]
    return json.loads(tok)  # basic string: TOML escapes are a JSON subset here

def encode_key(s: str) -> str:
    return json.dumps(s)

KEY = r"""('[^']*'|"(?:[^"\\]|\\.)*")"""
HEADER = re.compile(r"^(\s*\[projects\.)" + KEY + r"(\]\s*(?:#.*)?)$")
INLINE = re.compile(r"^(\s*)" + KEY + r"(\s*=.*)$")

translated = []
out = []
in_projects = False
for line in text.splitlines():
    stripped = line.strip()
    if stripped.startswith("[") and not stripped.startswith("[["):
        in_projects = stripped == "[projects]"
    m = HEADER.match(line)
    if m:
        old = decode_key(m.group(2))
        new = translate_path(old)
        if new:
            translated.append((old, new))
            line = m.group(1) + encode_key(new) + m.group(3)
    elif in_projects:
        m = INLINE.match(line)
        if m:
            old = decode_key(m.group(2))
            new = translate_path(old)
            if new:
                translated.append((old, new))
                line = m.group(1) + encode_key(new) + m.group(3)
    out.append(line)
text = "\n".join(out) + "\n"
data = tomllib.loads(text)

for old, new in translated:
    print(f"  ~ trusted folder {old!r} -> {new!r}")

# ---- 2. drop Windows-only settings, warn about Windows paths elsewhere -----
dropped = []
if sys.platform != "win32":
    for key in ("windows", "windows_wsl_setup_acknowledged"):
        if key in data:
            dropped.append(key)
            del data[key]
for key in dropped:
    print(f"  - [{key}] is Windows-only; not carried over")

def walk(obj, path):
    if isinstance(obj, dict):
        for k, v in obj.items():
            yield from walk(v, path + [k])
    elif isinstance(obj, list):
        for i, v in enumerate(obj):
            yield from walk(v, path + [str(i)])
    elif isinstance(obj, str):
        yield ".".join(path), obj

for path, value in walk(data, []):
    if path.startswith("projects."):
        continue
    if DRIVE.match(value) or value.startswith("\\\\"):
        print(f"  ! {path} = {value!r} looks like a Windows path; check it by hand")

# ---- 3. minimal TOML emitter (what the merge appends) ----------------------
def fmt(v):
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, (int, float)):
        return repr(v)
    if isinstance(v, str):
        return json.dumps(v)
    if isinstance(v, list):
        return "[" + ", ".join(fmt(x) for x in v) + "]"
    if isinstance(v, dict):
        return "{ " + ", ".join(f"{fmt_key(k)} = {fmt(x)}" for k, x in v.items()) + " }"
    return json.dumps(str(v))

def fmt_key(k):
    return k if re.fullmatch(r"[A-Za-z0-9_-]+", k) else json.dumps(k)

def emit_table(path, table):
    lines = []
    scalars = {k: v for k, v in table.items() if not isinstance(v, dict)}
    subs = {k: v for k, v in table.items() if isinstance(v, dict)}
    if path and (scalars or not subs):
        lines.append("[" + ".".join(fmt_key(p) for p in path) + "]")
    for k, v in scalars.items():
        lines.append(f"{fmt_key(k)} = {fmt(v)}")
    for k, v in subs.items():
        if lines and lines[-1] != "":
            lines.append("")
        lines.extend(emit_table(path + [k], v))
    return lines

def emit_top(key, value):
    if isinstance(value, dict):
        return emit_table([key], value)
    return [f"{fmt_key(key)} = {fmt(value)}"]

# ---- 4. write or merge -----------------------------------------------------
header = f"# --- migrated from {src_home} by migrate-codex-home.sh ({stamp}) ---"

if not os.path.exists(dst_path) or force:
    if dropped:
        body = "\n".join(line for key in data for line in emit_top(key, data[key]) + [""])
        text = header + "\n" + body
    print(f"  + config.toml written ({'replacing the old one' if force else 'new'})")
    if not dry_run:
        with open(dst_path, "w", encoding="utf-8") as f:
            f.write(text)
    sys.exit(0)

dst_text = open(dst_path, encoding="utf-8").read()
try:
    dst = tomllib.loads(dst_text)
except tomllib.TOMLDecodeError as e:
    sys.exit(f"migrate-codex-home: {dst_path} is not valid TOML: {e}")

MAP_TABLES = {"projects", "mcp_servers", "profiles", "model_providers", "permissions"}
added, kept = [], []
for key, value in data.items():
    if key not in dst:
        added.append((f"[{key}]" if isinstance(value, dict) else key, emit_top(key, value)))
    elif key in MAP_TABLES and isinstance(value, dict) and isinstance(dst[key], dict):
        for sub, subval in value.items():
            if sub in dst[key]:
                kept.append(f"{key}.{sub}")
            elif isinstance(subval, dict):
                added.append((f"[{key}.{fmt_key(sub)}]", emit_table([key, sub], subval)))
            else:
                added.append((f"{key}.{sub}", [f"[{key}]", f"{fmt_key(sub)} = {fmt(subval)}"]))
    else:
        kept.append(key)

for label, _ in added:
    print(f"  + {label} appended")
for label in kept:
    print(f"  = {label} already set in the destination; kept as is")
if not added:
    print("  (nothing to append)")
elif not dry_run:
    chunk = [header] + [line for _, lines in added for line in lines + [""]]
    with open(dst_path, "a", encoding="utf-8") as f:
        if not dst_text.endswith("\n"):
            f.write("\n")
        f.write("\n" + "\n".join(chunk).rstrip("\n") + "\n")
    tomllib.loads(open(dst_path, encoding="utf-8").read())  # must still parse
PY
  say ""
fi

say "not migrated on purpose: packages/ (install the fork with install-fork.sh), *.sqlite state, cache/, log/, tmp/, plugins/cache"
[ "$WITH_SESSIONS" = 1 ] || say "sessions and history were not copied; add --with-sessions to carry them over"
if [ "$DRY_RUN" = 1 ]; then
  say "dry run complete; rerun without --dry-run to apply"
else
  say "done. Verify with: CODEX_HOME=$DST codex --version"
fi
