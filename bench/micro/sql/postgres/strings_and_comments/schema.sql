CREATE TABLE real_table (
    id integer,
    note text
);

CREATE TABLE ghost (
    id integer
);

-- this view used to read FROM ghost
CREATE VIEW notes AS
SELECT id, 'select * from ghost' AS hint
FROM real_table;
