DROP TABLE orders CASCADE;

CREATE TABLE orders (
    id bigserial PRIMARY KEY,
    customer_id integer REFERENCES customers (id),
    status text
);
