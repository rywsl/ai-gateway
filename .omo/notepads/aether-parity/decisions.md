# Decisions — Aether Parity

## 2026-05-26 Session Start
- Phase order: D -> A -> C-spec -> C-impl -> B (Metis recommended)
- Each domain = separate PR
- TDD RED-GREEN-REFACTOR for all code tasks
- CLI variants go through full filter chain (auth/perm/ratelimit/billing), bypass IR only
- gw-billing handles task-level formulas only (freeze/settle by task_type+vendor+duration)
