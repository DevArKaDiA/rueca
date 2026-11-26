use rueca_extract::{Budget, LanguagePack, ProjectFiles};
use rueca_model::{FileFacts, Label, Language, RefKind};

use super::*;

fn facts(path: &str, src: &str) -> FileFacts {
    SqlPack.extract(path, Language::Sql, src.as_bytes(), &Budget::default()).unwrap()
}

/// `(label, qualified name)` of every definition.
fn defs(f: &FileFacts) -> Vec<(Label, String)> {
    f.definitions.iter().map(|d| (d.label, d.qualified_name.clone())).collect()
}

/// `(kind, receiver, name, enclosing qualified name)` of every reference.
fn refs(f: &FileFacts) -> Vec<(RefKind, String, String, String)> {
    f.references
        .iter()
        .map(|r| {
            let enc = r.enclosing.map_or("-".to_string(), |e| f.definitions[e as usize].qualified_name.clone());
            (r.kind, r.receiver.clone().unwrap_or_default(), r.name.clone(), enc)
        })
        .collect()
}

fn has_ref(f: &FileFacts, kind: RefKind, receiver: &str, name: &str, enclosing: &str) -> bool {
    refs(f).contains(&(kind, receiver.to_string(), name.to_string(), enclosing.to_string()))
}

#[test]
fn postgres_schema() {
    let src = "\
-- Customers of the shop.
CREATE TABLE public.customer (
    customer_id serial PRIMARY KEY,
    store_id integer NOT NULL REFERENCES store(store_id),
    email text
);

CREATE VIEW customer_list AS
SELECT cu.customer_id, cu.email, s.name FROM customer cu JOIN store s ON cu.store_id = s.store_id;

CREATE FUNCTION get_balance(p int) RETURNS numeric AS $$
BEGIN
  RETURN (SELECT sum(amount) FROM payment WHERE customer_id = p) + fee(p);
END
$$ LANGUAGE plpgsql;

CREATE TRIGGER last_updated BEFORE UPDATE ON customer FOR EACH ROW EXECUTE FUNCTION touch();
";
    let f = facts("db/schema.sql", src);
    assert_eq!(f.parse_errors, 0);
    let d = defs(&f);
    assert!(d.contains(&(Label::Struct, "customer".into())), "{d:?}");
    assert!(d.contains(&(Label::Field, "customer.email".into())));
    assert!(d.contains(&(Label::Struct, "customer_list".into())));
    assert!(d.contains(&(Label::Function, "get_balance".into())));
    assert!(d.contains(&(Label::Function, "customer.last_updated".into())), "{d:?}");
    let table = f.definitions.iter().find(|d| d.qualified_name == "customer").unwrap();
    assert_eq!(table.doc.as_deref(), Some("Customers of the shop."));
    assert!(table.signature.as_deref().unwrap().ends_with("-- postgres"), "{:?}", table.signature);
    assert!(has_ref(&f, RefKind::Read, "", "store", "customer"));
    assert!(has_ref(&f, RefKind::Read, "", "customer", "customer_list"));
    assert!(has_ref(&f, RefKind::Read, "$type:customer", "email", "customer_list"));
    assert!(has_ref(&f, RefKind::Read, "$type:store", "name", "customer_list"));
    assert!(has_ref(&f, RefKind::Read, "", "payment", "get_balance"));
    assert!(has_ref(&f, RefKind::Call, "", "fee", "get_balance"));
    assert!(has_ref(&f, RefKind::Read, "", "customer", "customer.last_updated"));
    assert!(has_ref(&f, RefKind::Call, "", "touch", "customer.last_updated"));
    // spans: the table's name
    let n = table.name_span;
    assert_eq!(&src[n.start_byte as usize..n.end_byte as usize], "customer");
}

#[test]
fn dml_writes_and_columns() {
    let src = "INSERT INTO orders (id, total) SELECT id, amount FROM carts;\nUPDATE orders SET total = 0 WHERE id = 1;\nDELETE FROM carts WHERE id = 2;\nDROP TABLE IF EXISTS old_orders;\nALTER TABLE orders ADD COLUMN note text;";
    let f = facts("q.sql", src);
    assert_eq!(f.parse_errors, 0);
    assert!(has_ref(&f, RefKind::Write, "", "orders", "-"));
    assert!(has_ref(&f, RefKind::Write, "$type:orders", "total", "-"));
    assert!(has_ref(&f, RefKind::Read, "", "carts", "-"));
    assert!(has_ref(&f, RefKind::Read, "$type:carts", "amount", "-"));
    assert!(has_ref(&f, RefKind::Write, "", "carts", "-"));
    assert!(has_ref(&f, RefKind::Write, "", "old_orders", "-"));
    assert!(defs(&f).contains(&(Label::Field, "orders.note".into())));
}

#[test]
fn mysql_procedures_with_delimiter() {
    let src = "\
CREATE TABLE `film` (`film_id` INT AUTO_INCREMENT, `title` VARCHAR(255), PRIMARY KEY (`film_id`)) ENGINE=InnoDB;
DELIMITER //
CREATE PROCEDURE film_in_stock(IN p_film_id INT)
BEGIN
  SELECT inventory_id FROM inventory WHERE film_id = p_film_id;
  CALL log_access(p_film_id);
END //
DELIMITER ;
";
    let f = facts("mysql/sakila.sql", src);
    assert!(defs(&f).contains(&(Label::Struct, "film".into())));
    assert!(defs(&f).contains(&(Label::Field, "film.title".into())));
    assert!(defs(&f).contains(&(Label::Function, "film_in_stock".into())));
    assert!(has_ref(&f, RefKind::Read, "", "inventory", "film_in_stock"));
    assert!(has_ref(&f, RefKind::Call, "", "log_access", "film_in_stock"));
}

#[test]
fn tsql_batches() {
    let src = "\
CREATE TABLE [dbo].[Orders] ([OrderID] INT NOT NULL, [CustomerID] NCHAR(5) NULL)
GO
CREATE PROCEDURE [dbo].[CustOrders] @CustomerID nchar(5)
AS
BEGIN
  SET NOCOUNT ON;
  SELECT OrderID FROM dbo.Orders WHERE CustomerID = @CustomerID;
  EXEC dbo.LogCall @CustomerID;
END
GO
";
    let f = facts("Northwind.sql", src);
    let d = defs(&f);
    assert!(d.contains(&(Label::Struct, "orders".into())), "{d:?}");
    assert!(d.contains(&(Label::Field, "orders.customerid".into())));
    assert!(d.contains(&(Label::Function, "custorders".into())));
    assert!(has_ref(&f, RefKind::Read, "", "orders", "custorders"), "{:?}", refs(&f));
    assert!(has_ref(&f, RefKind::Call, "", "logcall", "custorders"));
}

#[test]
fn sqlite_triggers() {
    let src = "CREATE TABLE city (id INTEGER PRIMARY KEY AUTOINCREMENT, last_update TIMESTAMP);\nCREATE TRIGGER city_trigger_au AFTER UPDATE ON city BEGIN UPDATE city SET last_update = DATETIME('NOW') WHERE rowid = new.rowid; END;";
    let f = facts("sqlite-sakila-schema.sql", src);
    assert!(defs(&f).contains(&(Label::Function, "city.city_trigger_au".into())));
    assert!(has_ref(&f, RefKind::Write, "", "city", "city.city_trigger_au"), "{:?}", refs(&f));
}

#[test]
fn bigquery_and_snowflake_names() {
    let f = facts(
        "bq/report.sql",
        "SELECT o.id FROM `proj.sales.orders` AS o QUALIFY ROW_NUMBER() OVER (PARTITION BY o.id) = 1",
    );
    assert!(has_ref(&f, RefKind::Read, "proj.sales", "orders", "-"), "{:?}", refs(&f));
    let f = facts("snowflake/x.sql", "CREATE TABLE \"ORDERS\" (ID INT);\nSELECT id FROM orders;");
    assert!(defs(&f).contains(&(Label::Struct, "orders".into())));
}

#[test]
fn dbt_models_and_macros() {
    let src = "{{ config(materialized='table') }}\nwith orders as (\n  select * from {{ ref('stg_orders') }}\n), payments as (\n  select * from {{ source('stripe', 'payment') }}\n)\nselect orders.order_id, {{ cents_to_dollars('payments.amount') }} as amount\nfrom orders left join payments using (order_id)\n";
    let f = facts("models/marts/orders.sql", src);
    let d = defs(&f);
    assert!(d.contains(&(Label::Struct, "orders".into())), "{d:?}");
    assert!(d.contains(&(Label::Variable, "orders.payments".into())));
    assert!(has_ref(&f, RefKind::Read, "", "stg_orders", "orders"), "{:?}", refs(&f));
    assert!(has_ref(&f, RefKind::Read, "stripe", "payment", "orders"));
    assert!(has_ref(&f, RefKind::Call, "", "cents_to_dollars", "orders"));
    assert!(has_ref(&f, RefKind::Read, "", "payments", "orders"));
    let m = facts(
        "macros/cents.sql",
        "{% macro cents_to_dollars(col, scale=2) %}\n  ({{ col }} / 100)::numeric(16, {{ scale }})\n{% endmacro %}\n",
    );
    assert_eq!(defs(&m), [(Label::Function, "cents_to_dollars".into())]);
    let e = facts(
        "models/x.sql",
        "select '{{ env_var(\"DBT_SCHEMA\") }}' as s, {{ var('start_date') }} as d from {{ ref('y') }}",
    );
    assert!(has_ref(&e, RefKind::EnvVar, "", "DBT_SCHEMA", "x"));
    assert!(has_ref(&e, RefKind::ConfigKey, "", "vars.start_date", "x"));
}

#[test]
fn named_queries() {
    let src = "-- name: GetAuthor :one\nSELECT * FROM authors WHERE id = $1;\n\n-- name: ListBooks :many\nSELECT b.title FROM books b;\n";
    let f = facts("query.sql", src);
    assert_eq!(defs(&f), [(Label::Function, "GetAuthor".into()), (Label::Function, "ListBooks".into())]);
    assert!(has_ref(&f, RefKind::Read, "", "authors", "GetAuthor"));
    assert!(has_ref(&f, RefKind::Read, "$type:books", "title", "ListBooks"));
    let f = facts("query.sql", "-- name: SayHello :one\nselect * from say_hello($1);\n");
    assert!(has_ref(&f, RefKind::Call, "", "say_hello", "SayHello"), "{:?}", refs(&f));
}

#[test]
fn bad_statement_does_not_lose_the_file() {
    let src = "CREATE TABLE a (id int);\nTHIS IS NOT SQL AT ALL FROM b;\nCREATE TABLE c (id int);";
    let f = facts("x.sql", src);
    assert_eq!(f.parse_errors, 1);
    let d = defs(&f);
    assert!(d.contains(&(Label::Struct, "a".into())) && d.contains(&(Label::Struct, "c".into())));
    assert!(has_ref(&f, RefKind::Read, "", "b", "-"));
}

#[test]
fn includes_resolve() {
    let f = facts("db/init.sql", "\\i tables.sql\n\\ir ../seed/data.sql\n");
    assert_eq!(f.imports.len(), 2);
    let project = ProjectFiles {
        paths: ["db/init.sql", "db/tables.sql", "seed/data.sql"].into_iter().map(String::from).collect(),
        configs: Default::default(),
    };
    assert_eq!(SqlPack.resolve_import("db/init.sql", &f.imports[0], &project).as_deref(), Some("db/tables.sql"));
    assert_eq!(SqlPack.resolve_import("db/init.sql", &f.imports[1], &project).as_deref(), Some("seed/data.sql"));
}

#[test]
fn deterministic() {
    let src = "CREATE TABLE t (a int); SELECT a FROM t;";
    assert_eq!(facts("x.sql", src), facts("x.sql", src));
}
