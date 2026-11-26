CREATE TABLE orders (
    id serial PRIMARY KEY,
    placed_at date
);

CREATE TABLE recent (
    id integer
);

CREATE VIEW last_week AS
WITH recent AS (
    SELECT id FROM orders WHERE placed_at > now() - interval '7 days'
)
SELECT id FROM recent;
