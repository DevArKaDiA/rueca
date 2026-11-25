CREATE TABLE `film` (
  `film_id` INT UNSIGNED NOT NULL AUTO_INCREMENT,
  `title` VARCHAR(255) NOT NULL,
  PRIMARY KEY (`film_id`)
) ENGINE=InnoDB;

CREATE TABLE `inventory` (
  `inventory_id` INT UNSIGNED NOT NULL AUTO_INCREMENT,
  `film_id` INT UNSIGNED NOT NULL,
  PRIMARY KEY (`inventory_id`),
  CONSTRAINT `fk_inventory_film` FOREIGN KEY (`film_id`) REFERENCES `film` (`film_id`)
) ENGINE=InnoDB;

DELIMITER $$
CREATE PROCEDURE count_titles()
READS SQL DATA
BEGIN
  SELECT COUNT(*) FROM film;
END $$

CREATE PROCEDURE film_in_stock(IN p_film_id INT)
READS SQL DATA
BEGIN
  SELECT inventory_id FROM inventory WHERE film_id = p_film_id;
  CALL count_titles();
END $$
DELIMITER ;
