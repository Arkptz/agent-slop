---
author: owner
code_paths:
- crates/agent-slop-core/src/rules/**
- crates/agent-slop-integrations/src/plugins/**
date: 2026-10-25
owner: owner
priority: medium
status: todo
tags:
- extensibility
---
# Confirm ADR-002 and SPEC-001 drafts for the extension surfaces

<!-- Brief introduction: what this document is about -->

## Goal
The owner reads the two drafts in /tmp/agent-slop-dg-drafts/ (ADR-002 external plugins via subprocess JSON; SPEC-001 custom rule schema 1, tuning keys and plugin protocol 1) and either confirms them for creation through dg or returns edits. The decisions they record already shipped under the 2026-10-25 owner answers (Q1-Q6).
## Acceptance Criteria
- ADR-002 confirmed or amended, then created with dg new adr
- SPEC-001 confirmed or amended, then created with dg new spec
- Both linked to ADR-001 and to the code_paths of this task
- config_v3 SPEC-006 pointer to the new SPEC decided (stay as source, or switch)

## Notes
Drafts: /tmp/agent-slop-dg-drafts/ADR-002-external-plugins-via-subprocess-json.md and SPEC-001-custom-rules-and-plugin-protocol.md. They are human-gated: agents propose, the owner confirms, then dg-writer creates them.