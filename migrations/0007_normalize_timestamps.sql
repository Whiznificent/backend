-- Keep all stored timestamps in UTC RFC 3339 form with millisecond
-- precision. Existing application-generated ISO timestamps are accepted
-- by SQLite's date parser and normalized here.
UPDATE accounts
SET created_at = strftime('%Y-%m-%dT%H:%M:%fZ', created_at);

UPDATE positions
SET opened_at = strftime('%Y-%m-%dT%H:%M:%fZ', opened_at),
    closed_at = CASE
        WHEN closed_at IS NULL THEN NULL
        ELSE strftime('%Y-%m-%dT%H:%M:%fZ', closed_at)
    END;

UPDATE watchlist
SET added_at = strftime('%Y-%m-%dT%H:%M:%fZ', added_at);

UPDATE alerts
SET created_at = strftime('%Y-%m-%dT%H:%M:%fZ', created_at),
    triggered_at = CASE
        WHEN triggered_at IS NULL THEN NULL
        ELSE strftime('%Y-%m-%dT%H:%M:%fZ', triggered_at)
    END;

UPDATE auth_nonces
SET created_at = strftime('%Y-%m-%dT%H:%M:%fZ', created_at),
    expires_at = strftime('%Y-%m-%dT%H:%M:%fZ', expires_at);

UPDATE sessions
SET created_at = strftime('%Y-%m-%dT%H:%M:%fZ', created_at),
    expires_at = strftime('%Y-%m-%dT%H:%M:%fZ', expires_at);
