CREATE TABLE film (
  film_id INT NOT NULL AUTO_INCREMENT PRIMARY KEY,
  title VARCHAR(255)
) ENGINE=InnoDB;

CREATE TABLE film_text (
  film_id INT NOT NULL,
  title VARCHAR(255)
) ENGINE=InnoDB;

DELIMITER ;;
CREATE TRIGGER `ins_film` AFTER INSERT ON `film` FOR EACH ROW BEGIN
    INSERT INTO film_text (film_id, title) VALUES (new.film_id, new.title);
END;;
DELIMITER ;
