CREATE TABLE author (
    id serial PRIMARY KEY,
    name text NOT NULL
);

CREATE TABLE book (
    id serial PRIMARY KEY,
    author_id integer NOT NULL REFERENCES author (id),
    title text
);

CREATE VIEW book_list AS
SELECT b.title, a.name
FROM book b
JOIN author a ON a.id = b.author_id;
