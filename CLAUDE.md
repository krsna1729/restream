# restream

@AGENTS.md

All agent wiring is agent-neutral and lives in `AGENTS.md`. Canonical skill
bodies: `docs/agent-guidance/skills/<name>/SKILL.md`. Local Claude Code shims
in `.claude/skills/` are generated, not checked in — if skills are missing,
run `scripts/setup-agent-skills.sh`.
