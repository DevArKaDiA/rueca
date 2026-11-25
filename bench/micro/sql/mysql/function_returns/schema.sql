CREATE TABLE payment (
  payment_id INT AUTO_INCREMENT PRIMARY KEY,
  customer_id INT,
  amount DECIMAL(5,2)
) ENGINE=InnoDB;

DELIMITER //
CREATE FUNCTION customer_total(p_customer INT) RETURNS DECIMAL(10,2)
DETERMINISTIC
READS SQL DATA
BEGIN
  DECLARE v DECIMAL(10,2);
  SELECT SUM(amount) INTO v FROM payment WHERE customer_id = p_customer;
  RETURN v;
END //
DELIMITER ;

CREATE VIEW big_spenders AS
SELECT customer_id FROM payment GROUP BY customer_id HAVING customer_total(customer_id) > 100;
