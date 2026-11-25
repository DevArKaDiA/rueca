CREATE TABLE events (
    id integer,
    at timestamptz,
    kind text
);

CREATE FUNCTION classify(k text) RETURNS text
LANGUAGE sql AS $$ SELECT upper(k) $$;

CREATE VIEW event_counts AS
SELECT classify(kind) AS kind, count(*) AS n, coalesce(max(at), now()) AS last_at
FROM events
GROUP BY 1;
