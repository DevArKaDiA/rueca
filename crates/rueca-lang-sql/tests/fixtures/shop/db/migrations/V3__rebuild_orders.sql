DROP TABLE orders;

CREATE TABLE orders (
    id bigserial PRIMARY KEY,
    customer_id integer NOT NULL REFERENCES customers (id),
    status text NOT NULL DEFAULT 'new'
);
