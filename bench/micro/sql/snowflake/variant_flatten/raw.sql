CREATE TABLE raw_events (
  payload VARIANT,
  loaded_at TIMESTAMP_NTZ
);

CREATE VIEW parsed_events AS
SELECT e.payload:id::STRING AS id, f.value::STRING AS tag
FROM raw_events e,
LATERAL FLATTEN(input => e.payload:tags) f;
