import { readFile, writeFile } from "node:fs/promises";

const entries = JSON.parse(await readFile("docs/error-codes.json", "utf8"));
const lines = [
  "# API error catalogue",
  "",
  "This file is generated from `docs/error-codes.json`. Error codes are stable API identifiers; clients must not branch on message text.",
  "",
  "| Code | HTTP status | Meaning |",
  "|---|---:|---|",
  ...entries.map(({ code, http_status, description }) =>
    `| \`${code}\` | ${Array.isArray(http_status) ? http_status.join(", ") : http_status} | ${description} |`,
  ),
  "",
];
await writeFile("docs/error-catalogue.md", lines.join("\n"));
