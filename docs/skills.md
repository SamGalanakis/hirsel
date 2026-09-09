# Skills

Skills are local directories containing a `SKILL.md` file with YAML metadata
and Markdown instructions. Hirsel lists their names and descriptions in the
Agent's guidance; the Agent reads the instructions with `shell.run` when needed.
Skills can refer to scripts, examples, and other files in their own directory.

Invoke one explicitly in any Thread's composer:

```text
/skill:code-review review the changes in /workspace/code/my-project
```

The conversation keeps that short command. Hirsel saves the expanded
instructions with the accepted Agent request, including the skill's directory
and your arguments. Editing or removing the file afterwards does not change
that queued request. Unknown or invalid skills return an error before accepting
the message. Ordinary text and generated instrument action labels are not
expanded. The same command works through the existing native-client message
protocol; there is no separate skill execution service.

## Locations

Hirsel scans these roots in order; the first valid skill with a given name wins:

1. Directories in `HIRSEL_SKILL_DIRS`, in the supplied order.
2. `<HIRSEL_DATA_DIR>/skills` (`./data/skills` by default).
3. `.agents/skills` under the Host's working directory.
4. `~/.agents/skills`.

`HIRSEL_SKILL_DIRS` uses the platform's path-list separator (colon on Linux).
For example, add your existing Codex and Claude skills when starting the Host:

```bash
export HIRSEL_SKILL_DIRS="$HOME/.codex/skills:$HOME/.claude/skills"
```

Paths belong to the Host machine. Thread focus does not select a repository
or change skill roots. Restart the Host after changing the environment; file
edits and new skills within those roots appear on the next Agent turn or
explicit invocation. You can ask the Agent which skills are available.

Discovery follows directory symlinks, deduplicates canonical directories, and
stops below a directory containing `SKILL.md`. It skips hidden subdirectories
and scans at most 16 directory levels below each root. Duplicate names and
invalid skills produce diagnostic warnings; they do not prevent Host startup.
Each skill file is limited to 256 KiB. Hirsel does not install packages or
execute scripts during discovery.

## Format

```markdown
---
name: code-review
description: Review a repository diff for bugs and missing regression coverage.
---

Read references/checklist.md, inspect the requested diff, and report concrete
bugs with file locations and suggested verification.
```

`name` is 1–64 lowercase letters, digits, or hyphens, without leading,
trailing, or repeated hyphens. `description` is 1–1024 characters. YAML quoted
strings and multiline descriptions work. Names need not match directory names.
Resolve relative references against the directory containing `SKILL.md`.

Add `disable-model-invocation: true` to omit a skill from the Agent's automatic
discovery guidance while retaining explicit `/skill:name` invocation. Other
metadata is ignored; it does not grant tools or change execution permissions.

Existing in-tree plugin instruction packs continue to use the plugin mechanism
documented in [plugins.md](plugins.md).
