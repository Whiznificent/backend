# API error catalogue

This file is generated from `docs/error-codes.json`. Error codes are stable API identifiers; clients must not branch on message text.

| Code | HTTP status | Meaning |
|---|---:|---|
| `CLIENT_ERROR` | 400 | Unclassified client-side failure. |
| `CONFLICT` | 409 | The request conflicts with current resource state. |
| `FORBIDDEN` | 403 | The authenticated caller is not permitted to perform this operation. |
| `INSUFFICIENT_BALANCE` | 422 | The account balance or available collateral is insufficient. |
| `INTERNAL_ERROR` | 500 | Unexpected server-side failure. |
| `INVALID_CREDENTIALS` | 400, 401 | A supplied wallet address or signature credential is invalid. |
| `INVALID_REQUEST` | 400 | The request is malformed or has invalid fields. |
| `NOT_FOUND` | 404 | The requested resource does not exist or is not visible to this caller. |
| `RATE_LIMITED` | 429 | The caller exceeded the configured request rate. |
| `SERVICE_UNAVAILABLE` | 503 | A required dependency is unavailable. |
| `UNAUTHENTICATED` | 401 | Authentication is missing, invalid, expired, or failed. |
| `UNPROCESSABLE_ENTITY` | 422 | The request is syntactically valid but cannot be processed. |
