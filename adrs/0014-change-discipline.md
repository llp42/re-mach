# Reviewable changes

Keep each change small and focused on one behavior or concern, and commit
mechanical edits, such as formatting and unrelated renames, separately from
behavior changes. This keeps review and regression diagnosis reliable.
[ADR 0006](0006-validation-milestones.md) defines the validation a change
requires.

- Explain the problem, resulting behavior, and validation in the commit
  message or change description, including commands, outcomes, and checks
  not run.
- Address new compiler warnings. Keep necessary lint suppressions local and
  explain why.
- Review unsafe operations, boot boundaries, and public ABI changes against
  their contracts.
- Preserve source provenance when adapting existing code
  ([ADR 0011](0011-licensing-and-provenance.md)).
- Update affected ADRs in the same change, following the process in the
  [index](README.md).
