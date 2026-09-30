# Zero-Downtime Migration Policy (Expand / Contract)

## Guidelines
1. **Never Drop or Rename Columns Directly**: Use the expand/contract parallel change pattern across two release cycles.
2. **Nullable or Default Values**: Always supply a `DEFAULT` for newly added `NOT NULL` columns.
3. **Down Migrations**: Every `.sql` migration must have a matching `.down.sql` file.
4. **Linter Annotations**: If a destructive operation is strictly required, annotate with `-- lint:allow reason: <explanation>`.
