CREATE TABLE customers (
    id serial PRIMARY KEY
);

CREATE TABLE orders (
    id serial PRIMARY KEY,
    customer_id integer REFERENCES customers (id)
);
