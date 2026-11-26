CREATE TABLE orders (
    id serial PRIMARY KEY,
    cents integer NOT NULL
);

CREATE FUNCTION cents_to_euros(c integer) RETURNS numeric
LANGUAGE sql AS $$ SELECT c / 100.0 $$;

CREATE FUNCTION order_total(p_id integer) RETURNS numeric AS $$
DECLARE
    v integer;
BEGIN
    SELECT cents INTO v FROM orders WHERE id = p_id;
    RETURN cents_to_euros(v);
END
$$ LANGUAGE plpgsql;
