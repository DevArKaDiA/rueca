-- Shop schema, first migration.
CREATE TABLE customers (
    id serial PRIMARY KEY,
    name text NOT NULL
);

-- Orders placed by customers.
CREATE TABLE orders (
    id serial PRIMARY KEY,
    customer_id integer NOT NULL REFERENCES customers (id),
    placed_at timestamptz NOT NULL DEFAULT now()
);
