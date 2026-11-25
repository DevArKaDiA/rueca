CREATE TABLE customer (
  customer_id NUMBER(10) NOT NULL,
  email VARCHAR2(100 CHAR),
  CONSTRAINT pk_customer PRIMARY KEY (customer_id)
);

CREATE TABLE payment (
  payment_id NUMBER(10) NOT NULL,
  customer_id NUMBER(10) NOT NULL,
  amount NUMBER(8,2),
  last_update DATE,
  CONSTRAINT fk_payment_customer FOREIGN KEY (customer_id) REFERENCES customer (customer_id)
);

CREATE OR REPLACE FUNCTION customer_total(p_customer IN NUMBER) RETURN NUMBER IS
  l_total NUMBER;
BEGIN
  SELECT SUM(amount) INTO l_total FROM payment WHERE customer_id = p_customer;
  RETURN NVL(l_total, 0);
END customer_total;
/

CREATE OR REPLACE PROCEDURE purge_customer(p_customer IN NUMBER) IS
BEGIN
  IF customer_total(p_customer) = 0 THEN
    DELETE FROM payment WHERE customer_id = p_customer;
    DELETE FROM customer WHERE customer_id = p_customer;
  END IF;
END;
/

CREATE OR REPLACE TRIGGER payment_before_update
BEFORE UPDATE ON payment
FOR EACH ROW
BEGIN
  :NEW.last_update := SYSDATE;
END;
/

CREATE OR REPLACE VIEW big_spenders AS
SELECT c.customer_id, c.email FROM customer c WHERE customer_total(c.customer_id) > 100;
