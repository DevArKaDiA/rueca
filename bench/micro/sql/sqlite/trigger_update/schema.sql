CREATE TABLE city (
  city_id INTEGER PRIMARY KEY AUTOINCREMENT,
  last_update TIMESTAMP
);

CREATE TRIGGER city_trigger_au AFTER UPDATE ON city
BEGIN
  UPDATE city SET last_update = DATETIME('NOW') WHERE rowid = new.rowid;
END;
