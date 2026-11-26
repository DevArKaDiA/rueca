CREATE TABLE order_items (
    order_id integer NOT NULL REFERENCES orders (id),
    sku text NOT NULL,
    cents integer NOT NULL
);

ALTER TABLE customers ADD COLUMN email text;

CREATE FUNCTION order_total(p_order integer) RETURNS integer AS $$
BEGIN
    RETURN (SELECT sum(cents) FROM order_items WHERE order_id = p_order);
END
$$ LANGUAGE plpgsql;
