CREATE TABLE order_items (
    order_id integer REFERENCES orders (id),
    sku text
);
