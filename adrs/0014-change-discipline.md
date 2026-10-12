# Reviewable changes

Keep changes small and focused on one behavior or concern. Separating
mechanical edits from functional changes makes review and regression
diagnosis more reliable.

- Commit formatting and unrelated renames separately from behavior changes.
- Explain the problem, resulting behavior, and validation in commit messages
  or change descriptions; include commands, outcomes, and checks not run.
- Address new compiler warnings. Keep necessary lint suppressions local and
  explain their reason.
- Review unsafe operations, boot boundaries, and public ABI changes against
  their contracts. Preserve source provenance when adapting existing code.
- Update affected ADRs in the same change, following the process in the
  [index](README.md).

Validation requirements are defined in
[ADR 0006](0006-validation-milestones.md).
