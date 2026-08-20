First, read `./skills/SKILL.md` for script-first development guidance.

You are testing the eguidev agent surface over MCP. The launcher (`edev mcp`)
exposes `start`, `stop`, `restart`, and `status`. The attached app exposes
`script_api` and `script_eval`. Do not expect removed per-endpoint widget,
action, or fixture tools.

For each exercise:

- Start the app through `start` if it is not already running.
- Call `script_api` when you need the live Luau contract.
- Drive setup, interaction, waits, and verification inside one `script_eval`.
- Confirm the structured outcome (`success`, `value`, `assertions`, `error`)
  matches what the script should produce.

See `testing/dev.md` for the demo-app exercise list.
