CREATE TABLE film (
    id serial PRIMARY KEY,
    last_update timestamptz
);

CREATE FUNCTION touch() RETURNS trigger AS $$
BEGIN
    NEW.last_update = now();
    RETURN NEW;
END
$$ LANGUAGE plpgsql;

CREATE TRIGGER film_touch BEFORE UPDATE ON film
    FOR EACH ROW EXECUTE FUNCTION touch();
