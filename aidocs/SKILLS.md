# Project Skills

Custom slash commands (skills) available in this repository.
Invoke with `/skill-name [args]` inside Claude Code.

---

## `/frontend-design` — HTTP frontend work

**When to use:** Any time you need to add or modify UI in the HTTP web frontend — new control rows, panels, visual polish, capability-gated elements, or JS behaviour wired to REST endpoints.

**What it loads:** Design system context (palette, layout primitives, patterns), key file paths, and coding conventions so Claude writes code that matches the existing UI without needing to re-read the style guide each time.

**File:** `.claude/commands/frontend-design.md`

**Example invocations**

```
/frontend-design Add a CW keyer speed row (wpm slider) that POSTs to /set_cw_wpm, shown only when capabilities.tx is true.
/frontend-design Polish the filters panel — align the bandwidth label with the FIR taps label and add a unit suffix to the slider readout.
/frontend-design Add a waterfall canvas below the signal meter that renders frequency vs. time from a new SSE stream.
```

---

## Adding new skills

Place a Markdown file in `.claude/commands/<skill-name>.md`.
Use `$ARGUMENTS` as the placeholder for user-supplied text.
Skills in `.claude/commands/` are project-scoped and not committed if `.claude/` is in `.gitignore`.

To make a skill part of the repo (shared with all contributors), add it to `aidocs/` as documentation and track the command file in version control by removing `.claude/` from `.gitignore` or adding a specific exception.

---

## Global skills (available in all projects)

| Skill | When to use |
|-------|------------|
| `frontend-design` | Also installed globally; project version takes precedence here |
| `keybindings-help` | Customise Claude Code keyboard shortcuts |
