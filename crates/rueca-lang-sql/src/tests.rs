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
    assert!(has_ref(&f, RefKind::Read, "proj.sales", "orders", "report.sql"), "{:?}", refs(&f));
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

#[test]
fn tsql_procedures_statement_by_statement() {
    // No semicolons, `ALTER PROCEDURE`, control flow, a cursor, `INSERT …
    // EXEC`, and one statement nothing parses: the rest is still read.
    let src = "\
ALTER PROCEDURE dbo.RefreshTotals @Day DATE = NULL, @Force BIT = 0
WITH RECOMPILE
AS
BEGIN
    SET NOCOUNT ON
    DECLARE @n INT
    IF @Day IS NULL SET @Day = GETDATE()
    BEGIN TRY
        INSERT INTO dbo.Totals (Day, Amount)
        SELECT o.Day, SUM(o.Amount) FROM dbo.Orders o WITH (NOLOCK) WHERE o.Day = @Day GROUP BY o.Day OPTION (RECOMPILE)
        INSERT #log EXEC dbo.WriteAudit @Day, N'totals'
        DECLARE c CURSOR LOCAL FAST_FORWARD FOR SELECT Id FROM dbo.Customers
        OPEN c
        WHILE @@FETCH_STATUS = 0
        BEGIN
            FETCH NEXT FROM c INTO @n
            UPDATE dbo.Customers SET Seen += 1 WHERE Id = @n
        END
        CLOSE c DEALLOCATE c
        SELECT TOP 1 FROM WHERE
    END TRY
    BEGIN CATCH
        EXEC dbo.LogError @Proc = N'RefreshTotals'
        THROW
    END CATCH
    RETURN 0
END
GO
sp_recompile N'dbo.Totals'
GO
";
    let f = facts("sql/procs.sql", src);
    assert_eq!(f.parse_errors, 1, "one statement of the body is not SQL");
    assert!(defs(&f).contains(&(Label::Function, "refreshtotals".into())), "{:?}", defs(&f));
    let r = refs(&f);
    for (kind, name) in [
        (RefKind::Write, "totals"),
        (RefKind::Read, "orders"),
        (RefKind::Call, "writeaudit"),
        (RefKind::Read, "customers"),
        (RefKind::Write, "customers"),
        (RefKind::Call, "logerror"),
    ] {
        assert!(has_ref(&f, kind, "", name, "refreshtotals"), "{kind:?} {name}: {r:?}");
    }
    assert!(has_ref(&f, RefKind::Write, "$type:totals", "amount", "refreshtotals"), "columns of parsed DML: {r:?}");
    assert!(has_ref(&f, RefKind::Call, "", "sp_recompile", "-"), "{r:?}");
    assert!(!r.iter().any(|x| x.2 == "@n" || x.2 == "c"), "variables and cursors are not relations: {r:?}");
}

#[test]
fn tsql_scripts_and_inline_functions() {
    let src = "\
IF OBJECT_ID('dbo.Stage') IS NOT NULL DROP TABLE dbo.Stage
CREATE TABLE dbo.Stage (Id INT IDENTITY(1,1) PRIMARY KEY CLUSTERED, Total AS Price * Qty, Price MONEY, Qty INT) ON [PRIMARY]
ALTER TABLE dbo.Stage ADD CONSTRAINT DF_Stage_Qty DEFAULT (1) FOR Qty
SET STATISTICS TIME, IO OFF
GO
CREATE FUNCTION dbo.OpenOrders (@c INT) RETURNS TABLE AS
RETURN WITH o AS (SELECT * FROM dbo.Orders WHERE Customer = @c) SELECT o.Id FROM o INNER HASH JOIN dbo.Stage s ON s.Id = o.Id
GO
";
    let f = facts("mssql/stage.sql", src);
    assert_eq!(f.parse_errors, 0, "{:?}", unparsed_statements("mssql/stage.sql", src.as_bytes()));
    let d = defs(&f);
    assert!(d.contains(&(Label::Struct, "stage".into())), "{d:?}");
    assert!(d.contains(&(Label::Field, "stage.total".into())), "computed column: {d:?}");
    assert!(d.contains(&(Label::Function, "openorders".into())));
    assert!(has_ref(&f, RefKind::Write, "", "stage", "-"));
    assert!(has_ref(&f, RefKind::Read, "", "orders", "openorders"), "{:?}", refs(&f));
    assert!(has_ref(&f, RefKind::Read, "", "stage", "openorders"));
}

#[test]
fn oracle_packages_and_sqlplus_scripts() {
    let src = "\
set serveroutput on
prompt Installing billing
@@common/types.sql
whenever sqlerror exit failure

create table invoice (id number(10) not null, total number(12,2), note varchar2(200 char))
  pctfree 10 tablespace users;

create or replace package billing authid definer as
  function total_for(p_customer in number) return number;
  procedure close_invoice(p_id in invoice.id%type);
end billing;
/
create or replace package body billing as
  g_calls pls_integer := 0;

  function total_for(p_customer in number) return number is
    l_total number;
  begin
    select sum(i.total) into l_total from invoice i where i.customer_id = p_customer;
    return nvl(l_total, 0);
  end total_for;

  procedure close_invoice(p_id in invoice.id%type) is
    cursor c_lines is select line_id from invoice_line where invoice_id = p_id;
  begin
    for r in c_lines loop
      audit_pkg.log_line(r.line_id);
    end loop;
    if total_for(p_id) > 0 then
      update invoice set note = 'closed' where id = p_id returning total into g_calls;
    elsif sql%rowcount = 0 then
      raise_application_error(-20001, 'no invoice');
    end if;
  exception
    when no_data_found then
      null;
  end close_invoice;
end billing;
/
begin
  billing.close_invoice(1);
end;
/
";
    let f = facts("db/install.sql", src);
    assert_eq!(detect_dialect("db/install.sql", src).0, Dialect::Oracle);
    assert_eq!(f.parse_errors, 0, "{:?}", unparsed_statements("db/install.sql", src.as_bytes()));
    assert_eq!(f.imports.iter().map(|i| i.module.as_str()).collect::<Vec<_>>(), ["common/types.sql"]);
    let d = defs(&f);
    assert!(d.contains(&(Label::Struct, "invoice".into())), "{d:?}");
    assert!(d.contains(&(Label::Field, "invoice.note".into())), "{d:?}");
    assert_eq!(d.iter().filter(|x| **x == (Label::Module, "billing".to_string())).count(), 2, "spec and body: {d:?}");
    assert_eq!(d.iter().filter(|x| **x == (Label::Function, "billing.total_for".to_string())).count(), 2);
    let decl: Vec<bool> = f
        .definitions
        .iter()
        .filter(|x| x.qualified_name == "billing.close_invoice")
        .map(|x| x.is_declaration)
        .collect();
    assert_eq!(decl, [true, false]);
    let r = refs(&f);
    assert!(has_ref(&f, RefKind::Read, "", "invoice", "billing.total_for"), "{r:?}");
    assert!(has_ref(&f, RefKind::Read, "", "invoice_line", "billing.close_invoice"), "cursor query: {r:?}");
    assert!(has_ref(&f, RefKind::Call, "audit_pkg", "log_line", "billing.close_invoice"), "{r:?}");
    assert!(has_ref(&f, RefKind::Call, "", "total_for", "billing.close_invoice"), "condition: {r:?}");
    assert!(has_ref(&f, RefKind::Write, "", "invoice", "billing.close_invoice"), "{r:?}");
    assert!(has_ref(&f, RefKind::Call, "billing", "close_invoice", "-"), "anonymous block: {r:?}");
    assert!(!r.iter().any(|x| x.2 == "l_total" || x.2 == "g_calls"), "INTO variables are not tables: {r:?}");
}

#[test]
fn oracle_plsql_extensions_and_errors() {
    assert_eq!(Language::from_path("source/core/ut_utils.pkb"), Some(Language::Sql));
    assert_eq!(Language::from_path("source/core/types/ut_key.tps"), Some(Language::Sql));
    let src = "\
create or replace package body p as
  procedure a is
  begin
    select 1 from where;
    b;
  end;
end;
/
";
    let f = facts("src/p.pkb", src);
    assert_eq!(f.parse_errors, 1);
    assert!(defs(&f).contains(&(Label::Function, "p.a".into())));
    assert!(has_ref(&f, RefKind::Call, "", "b", "p.a"), "the statement after the bad one is read: {:?}", refs(&f));
}

#[test]
fn db2_triggers_and_terminators() {
    let src = "\
--#SET TERMINATOR @
CREATE TABLE actor (actor_id INT NOT NULL GENERATED BY DEFAULT AS IDENTITY (START WITH 1, INCREMENT BY 1), last_update DATE)@
CREATE OR REPLACE TRIGGER actor_before_update
BEFORE UPDATE ON actor
REFERENCING NEW AS new
FOR EACH ROW
BEGIN
  SET new.last_update = current_date;
END@
CREATE PROCEDURE purge (IN p_days INT)
LANGUAGE SQL
BEGIN
  DECLARE v_count INT DEFAULT 0;
  DELETE FROM actor_audit WHERE age_days > p_days;
  CALL log_purge(v_count);
END@
";
    let f = facts("db2-sakila-db/schema.sql", src);
    assert_eq!(detect_dialect("db2-sakila-db/schema.sql", src).0, Dialect::Db2);
    assert_eq!(f.parse_errors, 0, "{:?}", unparsed_statements("db2-sakila-db/schema.sql", src.as_bytes()));
    let d = defs(&f);
    assert!(d.contains(&(Label::Struct, "actor".into())), "{d:?}");
    assert!(d.contains(&(Label::Function, "actor.actor_before_update".into())), "{d:?}");
    assert!(d.contains(&(Label::Function, "purge".into())));
    assert!(has_ref(&f, RefKind::Write, "", "actor_audit", "purge"), "{:?}", refs(&f));
    assert!(has_ref(&f, RefKind::Call, "", "log_purge", "purge"));
    assert!(f.definitions.iter().any(|x| x.signature.as_deref().is_some_and(|s| s.ends_with("-- db2"))));
}

#[test]
fn mysql_routines_with_characteristics() {
    let src = "\
DELIMITER //
CREATE DEFINER=`root`@`localhost` FUNCTION get_balance(p_customer INT, p_date DATETIME) RETURNS DECIMAL(5,2)
    DETERMINISTIC
    READS SQL DATA
BEGIN
  DECLARE v_rent DECIMAL(5,2);
  DECLARE done INT DEFAULT 0;
  DECLARE cur CURSOR FOR SELECT amount FROM payment WHERE customer_id = p_customer;
  DECLARE CONTINUE HANDLER FOR NOT FOUND SET done = 1;
  SELECT IFNULL(SUM(film.rental_rate), 0) INTO v_rent FROM film, rental WHERE rental.customer_id = p_customer;
  read_loop: LOOP
    FETCH cur INTO v_rent;
    IF done THEN LEAVE read_loop; END IF;
  END LOOP;
  RETURN v_rent - late_fee(p_customer);
END //
DELIMITER ;
";
    let f = facts("mysql/routines.sql", src);
    assert_eq!(f.parse_errors, 0, "{:?}", unparsed_statements("mysql/routines.sql", src.as_bytes()));
    assert!(defs(&f).contains(&(Label::Function, "get_balance".into())));
    let r = refs(&f);
    assert!(has_ref(&f, RefKind::Read, "", "payment", "get_balance"), "{r:?}");
    assert!(has_ref(&f, RefKind::Read, "", "rental", "get_balance"), "{r:?}");
    assert!(has_ref(&f, RefKind::Call, "", "late_fee", "get_balance"), "{r:?}");
    assert!(
        !r.iter().any(|x| x.2 == "v_rent" || x.2 == "p_customer"),
        "variables and parameters are not columns: {r:?}"
    );
    assert!(has_ref(&f, RefKind::Read, "$type:payment", "amount", "get_balance"), "{r:?}");
}

#[test]
fn bigquery_scripts() {
    let src = "\
DECLARE cutoff DATE DEFAULT CURRENT_DATE();
FOR t IN (SELECT table_name FROM `proj.ds.INFORMATION_SCHEMA.TABLES`) DO
  IF t.table_name LIKE 'tmp_%' THEN
    DELETE FROM `proj.ds.audit` WHERE day < cutoff;
  ELSE
    CALL `proj.ds.refresh`(t.table_name);
  END IF;
END FOR;
BEGIN
  INSERT INTO `proj.ds.audit` (day) VALUES (cutoff);
EXCEPTION WHEN ERROR THEN
  SELECT @@error.message;
END;
SELECT 1;
";
    let f = facts("scripts/cleanup.bq", src);
    assert_eq!(f.parse_errors, 0, "{:?}", unparsed_statements("scripts/cleanup.bq", src.as_bytes()));
    assert_eq!(stats("scripts/cleanup.bq", src.as_bytes()).statements, 4);
    let r = refs(&f);
    assert!(r.iter().any(|x| x.0 == RefKind::Write && x.2 == "audit"), "{r:?}");
    assert!(r.iter().any(|x| x.0 == RefKind::Call && x.2 == "refresh"), "{r:?}");
    assert!(r.iter().any(|x| x.0 == RefKind::Read && x.2 == "tables"), "{r:?}");
}

#[test]
fn postgres_scripts_and_own_statements() {
    let src = "\
CREATE PROCEDURE @extschema@.run_maintenance(p_analyze boolean DEFAULT false) LANGUAGE plpgsql AS $$
BEGIN
  PERFORM @extschema@.check_parent();
  UPDATE part_config SET done = true;
END
$$;
DO $$ BEGIN PERFORM cleanup_old(); END $$;
CREATE TABLE copy_t (LIKE part_config INCLUDING ALL);
ALTER EXTENSION pg_partman ADD FUNCTION check_parent();
SELECT make_part(p_parent := 'public.t') AS made \\gset
SELECT :'made' FROM part_config;
ALTER TABLE part_config ALTER COLUMN done SET STATISTICS 100;
";
    let f = facts("sql/pg_partman.sql", src);
    assert_eq!(f.parse_errors, 0, "{:?}", unparsed_statements("sql/pg_partman.sql", src.as_bytes()));
    let d = defs(&f);
    assert!(d.contains(&(Label::Function, "run_maintenance".into())), "the placeholder schema is dropped: {d:?}");
    assert!(d.contains(&(Label::Struct, "copy_t".into())));
    let r = refs(&f);
    assert!(has_ref(&f, RefKind::Call, "", "check_parent", "run_maintenance"), "{r:?}");
    assert!(has_ref(&f, RefKind::Write, "", "part_config", "run_maintenance"), "{r:?}");
    assert!(has_ref(&f, RefKind::Call, "", "cleanup_old", "-"), "DO body: {r:?}");
    assert!(has_ref(&f, RefKind::Call, "", "make_part", "-"), "{r:?}");
    assert!(has_ref(&f, RefKind::Write, "", "part_config", "-"), "ALTER TABLE: {r:?}");
    let s = stats("sql/pg_partman.sql", src.as_bytes());
    assert_eq!((s.statements, s.unparsed), (7, 0));
    assert!(s.own >= 4, "procedure, DO, ALTER EXTENSION, ALTER TABLE: {s:?}");
}

#[test]
fn own_readers_do_not_accept_broken_sql() {
    // Queries, DML, tables and views must parse: never one of the pack's
    // own forms.
    for (path, src) in [
        ("a.sql", "SELECT a FROM t WHERE;"),
        ("a.sql", "CREATE TABLE t (a int,, b int);"),
        ("mssql/a.sql", "UPDATE t SET WHERE a = 1"),
        ("mssql/a.sql", "CREATE VIEW v AS SELECT FROM WHERE"),
        ("oracle/a.sql", "begin\n  insert into t values (1,;\nend;\n/\n"),
        ("mysql/a.sql", "ALTER TABLE t ADD c int NOT NUL;"),
        ("mssql/a.sql", "BEGIN\n    SELECT 1\nEND\nEND"),
        ("mssql/a.sql", "this is not sql"),
    ] {
        assert_eq!(facts(path, src).parse_errors, 1, "{path}: {src}");
    }
}

#[test]
fn dbt_template_output_and_loops() {
    let only_refs = "{{ ref('stg_a') }}\n{{ ref('stg_b') }}\n";
    let f = facts("models/marts/fct.sql", only_refs);
    assert_eq!(f.parse_errors, 0);
    assert!(has_ref(&f, RefKind::Read, "", "stg_a", "fct"), "{:?} {:?}", refs(&f), defs(&f));
    let looped = "\
{{ config(materialized='table', post_hook=\"{{ log_done() }}\") }}
with unioned as (
    {% for m in models %}
    select id, {{ extra_columns(m) }} name from {{ ref('base') }}
    {% if not loop.last %} union all {% endif %}
    {% endfor %}
)
select id, row_number() over (partition by id {{ partition_extra() }} order by name) as n from unioned
";
    let f = facts("models/marts/looped.sql", looped);
    assert_eq!(f.parse_errors, 0, "{:?}", unparsed_statements("models/marts/looped.sql", looped.as_bytes()));
    assert!(has_ref(&f, RefKind::Read, "", "base", "looped"));
    assert!(has_ref(&f, RefKind::Call, "", "extra_columns", "looped"), "{:?}", refs(&f));
    let stray = "{% macro m() %}select 1{% endmacro %}}\n";
    assert_eq!(facts("macros/m.sql", stray).parse_errors, 0, "text outside the macros of a macros file is not SQL");
}

#[test]
fn head_only_statements_are_strict_parse_errors() {
    // A statement kind with no production: known by its head, references
    // kept, and a parse error of the strict count.
    let src = "CREATE EVENT SESSION [s] ON SERVER ADD EVENT sqlserver.rpc_completed\nGO\nSELECT 1 FROM t\nGO\n";
    let f = facts("mssql/x.sql", src);
    assert_eq!(f.parse_errors, 1);
    let s = stats("mssql/x.sql", src.as_bytes());
    assert_eq!((s.statements, s.unparsed, s.unparsed_strict, s.head_only, s.productions), (2, 0, 1, 1, 0));
    // The same kind of statement read by a production is parsed.
    let src = "ALTER DATABASE db SET RECOVERY FULL\nGO\nEXEC dbo.Audit @Id = 1, @Note = N'x'\nGO\n";
    let f = facts("mssql/y.sql", src);
    assert_eq!(f.parse_errors, 0);
    assert!(has_ref(&f, RefKind::Call, "", "audit", "y.sql"), "{:?}", refs(&f));
    let s = stats("mssql/y.sql", src.as_bytes());
    assert_eq!((s.unparsed_strict, s.head_only), (0, 0));
    assert!(s.productions >= 1, "{s:?}");
    // A malformed statement of a production's form is not rescued.
    assert_eq!(facts("mssql/z.sql", "ALTER DATABASE db SET\nGO\n").parse_errors, 1);
}

#[test]
fn a_script_that_defines_nothing_is_a_function_named_after_its_file() {
    // a report query: the file is the unit, named after it, and encloses its references
    let f = facts(
        "pg/b4_btree_bloat.sql",
        "-- B-tree bloat\nselect relname from pg_class c join pg_index i on i.indexrelid = c.oid;\n",
    );
    assert_eq!(defs(&f), [(Label::Function, "b4_btree_bloat.sql".to_string())]);
    assert_eq!(f.definitions[0].name, "b4_btree_bloat");
    assert_eq!(f.definitions[0].signature.as_deref(), Some("script b4_btree_bloat -- postgres"));
    assert!(has_ref(&f, RefKind::Read, "", "pg_class", "b4_btree_bloat.sql"), "{:?}", refs(&f));
    // a file with a definition has no script function, and its loose statements stay unenclosed
    let f = facts("pg/schema.sql", "CREATE TABLE t (id int);\nINSERT INTO t VALUES (1);\n");
    assert!(defs(&f).iter().all(|(l, _)| *l != Label::Function), "{:?}", defs(&f));
    assert!(has_ref(&f, RefKind::Write, "", "t", "-"), "{:?}", refs(&f));
    // nothing read (comments only): no definition
    assert!(defs(&facts("pg/empty.sql", "-- nothing here\n")).is_empty());
}

#[test]
fn round_four_constructs() {
    // A macro alone after the CTE list is the main query; comments
    // before a placeholder do not change where it stands.
    let src = "\
with a as (
    -- first source
    {{ dbt_utils.union_relations(relations=[ref('x'), ref('y')]) }}
)
, b as (select * from a)

/* the export */
{{ the_tuva_project.create_json_object(
    table_ref='b'
) }}
";
    let f = facts("models/m.sql", src);
    assert_eq!(f.parse_errors, 0, "{:?}", unparsed_statements("models/m.sql", src.as_bytes()));
    assert!(has_ref(&f, RefKind::Read, "", "x", "m"), "{:?}", refs(&f));
    // The same placeholder in the middle of a statement is not a query.
    let bad = "with a as (select 1)\n{{ m() }}\nselect 2 from\n";
    assert_eq!(facts("models/n.sql", bad).parse_errors, 1);

    // `DELIMITER` is a client command only with a symbol delimiter.
    let copy = "COPY t FROM '/data/t.csv'\nDELIMITER ','\nCSV HEADER;\nSELECT 1;\n";
    assert_eq!(stats("pg/load.pgsql", copy.as_bytes()).statements, 2);
    let udf = "CREATE OR REPLACE FUNCTION ds.f(\n  string STRING,\n  delimiter STRING,\n  part INT64)\nAS (SPLIT(string, delimiter)[ORDINAL(part)]);\n";
    let s = stats("udfs/f.bq", udf.as_bytes());
    assert_eq!((s.statements, s.unparsed_strict), (1, 0));

    // T-SQL: several columns added by one `ADD`, tab-indented.
    let alter = "CREATE TABLE #p (id INT)\nGO\nALTER TABLE #p\n\t\tADD waiter_mode\tNVARCHAR(256),\n\t\t\towner_mode NVARCHAR(256),\n\t\t\tis_victim INT\nGO\n";
    let f = facts("mssql/a.sql", alter);
    assert_eq!(f.parse_errors, 0, "{:?}", unparsed_statements("mssql/a.sql", alter.as_bytes()));
    let d = defs(&f);
    for column in ["#p.waiter_mode", "#p.owner_mode", "#p.is_victim"] {
        assert!(d.contains(&(Label::Field, column.to_string())), "{column}: {d:?}");
    }
    // …and one column that is not a column definition is an error.
    let broken = "ALTER TABLE t\n\tADD a INT,\n\tb INT NOT NUL,\n\tc INT\nGO\n";
    assert_eq!(facts("mssql/b.sql", broken).parse_errors, 1);
}
